//! Protocol logic.

mod acb;
mod clock;
mod dgm;
pub(crate) mod join;
pub(crate) mod sync;

use crate::{
    app::app_log,
    profile::Profile,
    proto::{clock::Clock, join::JoinProtocol, sync::SyncProtocol},
};
use base64::{Engine, prelude::BASE64_STANDARD_NO_PAD};
use dgm::GroupMembership;
use iroh::{NodeAddr, NodeId, PublicKey, endpoint::Connection, protocol::Router};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{Mutex, mpsc::UnboundedReceiver};

/// View of protocol state for UI.
///
/// Polled periodically since we can't lock the state mutices during rendering.
#[derive(Debug, Clone, Default)]
pub struct ProtocolState {
    pub peers: Vec<NodeId>,
    pub group: Option<ProtocolGroupState>,
}

#[derive(Debug, Clone)]
pub struct ProtocolGroupState {
    pub join_code: String,
    pub id: u64,
    pub members: HashSet<NodeId>,
}

#[derive(Debug)]

pub struct Protocol {
    pub router: Router,
    sync: SyncProtocol,
    join: JoinProtocol,

    pub group: Mutex<Option<GroupMembership>>, // TODO: remove pub

    sync_peers: Arc<Mutex<HashMap<NodeId, sync::Peer>>>,
}

/// Internal protocol events for signaling.
pub enum ProtocolEvent {
    AddGroupMember(NodeId),

    SyncConnection(iroh::endpoint::Connection),
}

impl Protocol {
    pub async fn new(profile: &Profile) -> anyhow::Result<Arc<Self>> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        // create iroh endpoint
        let endpoint = iroh::Endpoint::builder()
            .discovery_n0()
            .secret_key(profile.secret_key().clone())
            .bind()
            .await?;

        let sync_peers = Arc::new(Mutex::new(HashMap::new()));

        // create protocols
        let sync = SyncProtocol::new(tx.clone());
        let join = JoinProtocol::new(tx.clone());

        // create iroh router
        let router = iroh::protocol::Router::builder(endpoint)
            .accept(SyncProtocol::ALPN, sync.clone())
            .accept(JoinProtocol::ALPN, join.clone())
            .spawn()
            .await?;

        let protocol = Arc::new(Self {
            router,
            sync,
            join,

            group: Mutex::new(None),

            sync_peers,
        });

        tokio::spawn({
            let protocol = protocol.clone();
            async move {
                while let Some(event) = rx.recv().await {
                    let res = match event {
                        ProtocolEvent::AddGroupMember(node_id) => {
                            protocol.add_group_member(node_id).await
                        }

                        ProtocolEvent::SyncConnection(connection) => {
                            tokio::spawn({
                                let protocol = protocol.clone();
                                async move {
                                    let res = protocol.sync_accept(connection).await;
                                    if let Err(e) = res {
                                        app_log!("error during sync_accept: {e:#}");
                                    }
                                }
                            });
                            Ok(())
                        }
                    };

                    if let Err(e) = res {
                        app_log!("error handling ProtocolEvent: {e:#}");
                    }
                }
            }
        });

        Ok(protocol)
    }

    pub async fn shutdown(&self) -> anyhow::Result<()> {
        self.router.shutdown().await?;
        Ok(())
    }

    pub async fn poll_state(&self) -> anyhow::Result<ProtocolState> {
        let peers = {
            let peers = self.sync_peers.lock().await;
            peers.keys().copied().collect::<Vec<_>>()
        };

        let group = {
            let join_code = {
                let node_id = self.router.endpoint().node_id();
                let code = self.join.get_code().await;

                let mut bytes = [0u8; 40];
                bytes[0..32].copy_from_slice(node_id.as_bytes());
                bytes[32..40].copy_from_slice(&code.to_be_bytes());

                BASE64_STANDARD_NO_PAD.encode(&bytes)
            };

            let group = self.group.lock().await;
            group.as_ref().map(|dgm| ProtocolGroupState {
                id: dgm.group_id(),
                members: dgm.evaluate_members(),
                join_code,
            })
        };

        Ok(ProtocolState { peers, group })
    }

    pub async fn create_group(&self) -> anyhow::Result<()> {
        let mut group = self.group.lock().await;
        if group.is_some() {
            anyhow::bail!("already in a group");
        }

        group.replace(GroupMembership::new(
            self.router.endpoint().secret_key().clone(),
        ));

        Ok(())
    }

    pub async fn add_group_member(self: &Arc<Self>, node_id: NodeId) -> anyhow::Result<()> {
        let new_members = {
            let mut group = self.group.lock().await;
            let group = group.as_mut().ok_or(anyhow::anyhow!("not in a group"))?;

            group.add_member(node_id);

            group.evaluate_members()
        };

        self.reconcile_group_members(new_members).await?;

        Ok(())
    }

    pub async fn remove_group_member(self: &Arc<Self>, node_id: NodeId) -> anyhow::Result<()> {
        let new_members = {
            let mut group = self.group.lock().await;
            let group = group.as_mut().ok_or(anyhow::anyhow!("not in a group"))?;

            group.remove_member(node_id);

            group.evaluate_members()
        };

        self.reconcile_group_members(new_members).await?;

        Ok(())
    }

    pub async fn join_node(&self, addr: NodeAddr, code: u64) -> anyhow::Result<()> {
        self.join.join(self.router.endpoint(), addr, code).await?;
        Ok(())
    }

    async fn reconcile_group_members(
        self: &Arc<Self>,
        members: HashSet<PublicKey>,
    ) -> anyhow::Result<()> {
        let my_id = self.router.endpoint().node_id();

        let sync_peers = {
            let sync_peers = self.sync_peers.lock().await;

            // disconnect removed members
            for (peer_id, peer) in sync_peers.iter() {
                if *peer_id == my_id {
                    continue;
                }

                if !members.contains(peer_id) {
                    app_log!("peer {peer_id} no longer in group, disconnecting");
                    peer.close();
                }
            }

            // copy the list of keys out and drop the mutex guard
            // this is necessary because sync.connect wants to lock the peers mutex
            sync_peers.keys().copied().collect::<Vec<_>>()
        };

        // connect to new members
        // TODO: ratelimit attempts?
        for member in members.iter().copied() {
            if member == my_id {
                continue;
            }

            if !sync_peers.contains(&member) {
                app_log!("[proto] member {member} not connected, attempting to connect");
                let protocol = self.clone();
                tokio::spawn(async move {
                    let res = protocol.sync_connect(member).await;
                    if let Err(e) = res {
                        app_log!("error connecting: {e:#}");
                    };
                });
            }
        }

        Ok(())
    }

    // connect to a node
    // long lived, should be spawned on a task
    async fn sync_connect(&self, node_id: NodeId) -> anyhow::Result<()> {
        app_log!("[proto] [-> {node_id}] connecting");

        // open a connection to the accepting node
        let connection = self
            .router
            .endpoint()
            .connect(NodeAddr::new(node_id), SyncProtocol::ALPN)
            .await?;
        app_log!("[proto] [-> {node_id}] connected");

        // open a bidirectional QUIC stream
        let (send_stream, recv_stream) = connection.open_bi().await?;
        app_log!("[proto] [-> {node_id}] opened stream");

        // spawn tasks to bridge streams and channels
        let (peer, mut msg_rx) = sync::handle_connection_streams(node_id, send_stream, recv_stream);

        // TODO
        app_log!("[proto] [-> {node_id}] sending handshake");
        peer.send(sync::Message::Handshake {
            group_id: 0,
            local_clock: Clock::new(),
        });

        // wait for reply handshake
        let handshake = msg_rx.recv().await;
        app_log!("[proto] [-> {node_id}] got reply handshake");
        match handshake {
            Some(message) => {
                //
            }
            None => {
                // stream closed
                peer.close();
            }
        }

        self.sync_handshake_complete(connection, peer, msg_rx).await
    }

    // accept a connection
    // long-lived, should be spawned on a task
    async fn sync_accept(&self, connection: iroh::endpoint::Connection) -> anyhow::Result<()> {
        // get the remote node id
        let node_id = connection.remote_node_id()?;

        app_log!("[proto] [-> {node_id}] accepted connection");

        // we expect the connecting peer to open a single bi-directional stream
        let (send_stream, recv_stream) = connection.accept_bi().await?;
        app_log!("[proto] [-> {node_id}] accepted stream");

        // spawn tasks to bridge streams and channels
        let (peer, mut msg_rx) = sync::handle_connection_streams(node_id, send_stream, recv_stream);

        // wait for handshake message
        let handshake = msg_rx.recv().await;
        app_log!("[proto] [-> {node_id}] received handshake");
        match handshake {
            Some(message) => {
                //
            }
            None => {
                // stream closed
                return Ok(());
            }
        }

        // send reply handshake
        app_log!("[proto] [-> {node_id}] sending reply handshake");
        peer.send(sync::Message::Handshake {
            group_id: 0,
            local_clock: Clock::new(),
        });

        self.sync_handshake_complete(connection, peer, msg_rx).await
    }

    // handle a sync peer after handshaking successfully
    // used by both ends of the connection
    // long-lived, should be spawned on a task
    async fn sync_handshake_complete(
        &self,
        connection: Connection,
        peer: sync::Peer,
        mut msg_rx: UnboundedReceiver<sync::Message>,
    ) -> anyhow::Result<()> {
        // get the remote node id
        let node_id = connection.remote_node_id()?;

        let cancel_token = peer.cancel_token.clone();

        // add to peer map
        {
            let mut peers = self.sync_peers.lock().await;
            peers.insert(node_id, peer);
        }

        loop {
            tokio::select! {
                msg = msg_rx.recv() => {
                    app_log!("message from peer {node_id}: {msg:?}");
                }

                // if the connection closes for any reason, cancel to start cleanup
                _ = connection.closed() => {
                    cancel_token.cancel();
                }

                // close connection when cancel token is cancelled
                _ = cancel_token.cancelled() => {
                    // explicitly close the connection
                    connection.close(0u32.into(), b"closed");

                    // remove from peer map
                    {
                        let mut peers = self.sync_peers.lock().await;
                        peers.remove(&node_id);
                    }

                    app_log!("[proto] [-> {node_id}] connection closed");

                    // only break once cancel token has been cancelled, to ensure cleanup happens
                    break;
                }
            }
        }

        Ok(())
    }
}
