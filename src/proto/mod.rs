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
use acb::{CausalBroadcast, SignedMessage};
use base64::{Engine, prelude::BASE64_STANDARD_NO_PAD};
use dgm::{GroupMembership, Operation};
use iroh::{NodeAddr, NodeId, PublicKey, endpoint::Connection, protocol::Router};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{
    Mutex,
    mpsc::{UnboundedReceiver, UnboundedSender},
};

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

    tx: UnboundedSender<ProtocolEvent>,

    pub group: Mutex<Option<GroupMembership>>, // TODO: remove pub

    sync_peers: Arc<Mutex<HashMap<NodeId, sync::Peer>>>,
}

/// Internal protocol events for signaling.
pub enum ProtocolEvent {
    AddGroupMember(NodeId),

    SyncConnection(iroh::endpoint::Connection),

    SyncMessages(Vec<SignedMessage<Operation>>),
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

            tx,

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

                        ProtocolEvent::SyncMessages(messages) => {
                            tokio::spawn({
                                let protocol = protocol.clone();
                                async move {
                                    let mut group = protocol.group.lock().await;
                                    // TODO: handle None
                                    let group = group.as_mut().unwrap();

                                    for msg in messages {
                                        group.acb.receive(&msg);
                                    }

                                    let res = protocol.deliver_group_messages(group).await;
                                    if let Err(e) = res {
                                        app_log!("error during deliver_group_messages: {e:#}");
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

            let message = group.add_member(node_id);
            self.broadcast_messages(&[message]).await?;

            group.evaluate_members()
        };

        self.reconcile_group_members(new_members).await?;

        Ok(())
    }

    pub async fn remove_group_member(self: &Arc<Self>, node_id: NodeId) -> anyhow::Result<()> {
        let new_members = {
            let mut group = self.group.lock().await;
            let group = group.as_mut().ok_or(anyhow::anyhow!("not in a group"))?;

            let message = group.remove_member(node_id);
            self.broadcast_messages(&[message]).await?;

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
                    // TODO
                    let res = protocol.sync_connect(member, true).await;
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
    async fn sync_connect(self: &Arc<Self>, node_id: NodeId, welcome: bool) -> anyhow::Result<()> {
        app_log!("[proto] [-> {node_id}] connecting");

        // get group state for handshake from group mutex
        let (my_group_id, handshake_message) = {
            let group = self.group.lock().await;
            let Some(group) = group.as_ref() else {
                anyhow::bail!("sync_connect failed, not in a group");
            };

            let group_id = group.group_id();
            let local_clock = group.acb.local_clock().clone();

            let handshake_message = if welcome {
                sync::Message::Welcome {
                    group_id,
                    local_clock,
                    messages: group.acb.received().cloned().collect(),
                }
            } else {
                sync::Message::Handshake {
                    group_id,
                    local_clock,
                }
            };

            (group_id, handshake_message)
        };

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

        // send handshake message
        app_log!("[proto] [-> {node_id}] sending handshake");
        peer.send(handshake_message);

        // wait for reply handshake
        // we expect to receive a Handshake message with matching group id
        let handshake = msg_rx.recv().await;
        app_log!("[proto] [-> {node_id}] received reply handshake");
        let remote_clock = match handshake {
            Some(sync::Message::Handshake {
                group_id,
                local_clock,
            }) => {
                if group_id != my_group_id {
                    peer.close();
                    anyhow::bail!("handshake failed, group id reply mismatched");
                }

                local_clock
            }
            Some(_) | None => {
                // invalid handshake or stream closed
                peer.close();
                anyhow::bail!("handshake failed");
            }
        };

        // continue to connected state
        self.sync_handshake_complete(connection, peer, msg_rx, remote_clock)
            .await
    }

    // accept a connection
    // long-lived, should be spawned on a task
    async fn sync_accept(
        self: &Arc<Self>,
        connection: iroh::endpoint::Connection,
    ) -> anyhow::Result<()> {
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
        let remote_clock = match handshake {
            Some(sync::Message::Handshake {
                group_id,
                local_clock,
            }) => {
                let group = self.group.lock().await;

                match group.as_ref() {
                    // handshake and already in group, check group id matches
                    Some(group) => {
                        if group_id != group.group_id() {
                            peer.close();
                            anyhow::bail!("handshake failed, group id mismatched");
                        }

                        local_clock
                    }
                    // handshake and not in group, join it
                    // this shouldn't happen (we expect a Welcome message),
                    // but it's possible if we lose our local state for example
                    None => local_clock,
                }
            }
            Some(sync::Message::Welcome {
                group_id,
                local_clock,
                messages,
            }) => {
                let mut group = self.group.lock().await;

                match group.as_mut() {
                    // welcome and already in group, check group id matches
                    Some(group) => {
                        if group_id != group.group_id() {
                            peer.close();
                            anyhow::bail!(
                                "handshake failed, got welcome but already in different group"
                            );
                        }

                        // group id matched, so we have been welcomed twice
                        // receive welcome messages anyway
                        for msg in messages {
                            group.acb.receive(&msg);
                        }

                        // deliver messages and reconcile connections
                        self.deliver_group_messages(group).await?;

                        local_clock
                    }
                    // welcome and not in group, join it
                    None => {
                        // initialize acb state
                        let acb = CausalBroadcast::from_messages(
                            self.router.endpoint().secret_key().clone(),
                            local_clock.clone(),
                            messages,
                        );

                        // initialize group
                        let mut new_group = GroupMembership::from_causal_broadcast(acb);

                        // deliver messages and reconcile conections
                        self.deliver_group_messages(&mut new_group).await?;

                        // update group mutex
                        *group = Some(new_group);

                        local_clock
                    }
                }
            }
            Some(_) | None => {
                // invalid handshake or stream closed
                peer.close();
                anyhow::bail!("handshake failed");
            }
        };

        // send reply handshake
        app_log!("[proto] [-> {node_id}] sending reply handshake");
        {
            let group = self.group.lock().await;
            let Some(group) = group.as_ref() else {
                anyhow::bail!("sync_accept failed, not in a group when replying");
            };

            peer.send(sync::Message::Handshake {
                group_id: group.group_id(),
                local_clock: group.acb.local_clock().clone(),
            });
        }

        // continue to connected state
        self.sync_handshake_complete(connection, peer, msg_rx, remote_clock)
            .await
    }

    // handle a sync peer after handshaking successfully
    // used by both ends of the connection
    // long-lived, should be spawned on a task
    async fn sync_handshake_complete(
        self: &Arc<Self>,
        connection: Connection,
        peer: sync::Peer,
        mut msg_rx: UnboundedReceiver<sync::Message>,
        remote_clock: Clock,
    ) -> anyhow::Result<()> {
        // get the remote node id
        let node_id = connection.remote_node_id()?;

        let cancel_token = peer.cancel_token.clone();

        // add to peer map
        {
            let mut peers = self.sync_peers.lock().await;
            peers.insert(node_id, peer);
        }

        // TODO: initiate sync w known remote_clock from handshake

        loop {
            tokio::select! {
                Some(msg) = msg_rx.recv() => {
                    match msg {
                        // deliver messages and reconcile connections
                        sync::Message::Sync { messages } => {
                            app_log!("[proto] [-> {node_id}] received sync of {} messages", messages.len());
                            self.tx.send(ProtocolEvent::SyncMessages(messages)).unwrap();
                        }

                        sync::Message::Handshake {..} | sync::Message::Welcome {..} => {
                            app_log!("unexpected message");
                        }

                        msg => {
                            app_log!("message from peer {node_id}: {msg:?}");
                        }
                    }
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

    // attempt to deliver all ACB messages and reconcile group members if any messages were delivered
    async fn deliver_group_messages(
        self: &Arc<Self>,
        group: &mut GroupMembership,
    ) -> anyhow::Result<()> {
        let mut changed = false;
        while let Some(_next) = group.acb.deliver_next() {
            changed = true;
        }

        if changed {
            let members = group.evaluate_members();
            self.reconcile_group_members(members).await
        } else {
            Ok(())
        }
    }

    async fn broadcast_messages(
        &self,
        messages: &[SignedMessage<Operation>],
    ) -> anyhow::Result<()> {
        let peers = self.sync_peers.lock().await;

        for (_peer_id, peer) in peers.iter() {
            peer.send(sync::Message::Sync {
                messages: messages.to_vec(),
            });
        }

        Ok(())
    }
}
