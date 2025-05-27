//! Protocol logic.

mod acb;
mod clock;
mod dgm;
pub(crate) mod join;
pub(crate) mod sync;

use crate::{
    app::app_log,
    profile::Profile,
    proto::{join::JoinProtocol, sync::SyncProtocol},
};
use base64::{Engine, prelude::BASE64_STANDARD_NO_PAD};
use dgm::GroupMembership;
use iroh::{NodeAddr, NodeId, PublicKey, protocol::Router};
use std::{collections::HashSet, sync::Arc};
use tokio::sync::{Mutex, mpsc};

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
}

/// Internal protocol events for signaling.
pub enum ProtocolEvent {
    AddGroupMember(NodeId),
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

        // create protocols
        let sync = SyncProtocol::new();
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
        });

        tokio::spawn({
            let protocol = protocol.clone();
            async move {
                while let Some(event) = rx.recv().await {
                    let res = match event {
                        ProtocolEvent::AddGroupMember(node_id) => {
                            protocol.add_group_member(node_id).await
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
            let peers = self.sync.peers().lock().await;
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

    pub async fn add_group_member(&self, node_id: NodeId) -> anyhow::Result<()> {
        let new_members = {
            let mut group = self.group.lock().await;
            let group = group.as_mut().ok_or(anyhow::anyhow!("not in a group"))?;

            group.add_member(node_id);

            group.evaluate_members()
        };

        self.reconcile_group_members(new_members).await?;

        Ok(())
    }

    pub async fn remove_group_member(&self, node_id: NodeId) -> anyhow::Result<()> {
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

    async fn reconcile_group_members(&self, members: HashSet<PublicKey>) -> anyhow::Result<()> {
        let my_id = self.router.endpoint().node_id();

        let sync_peers = {
            let sync_peers = self.sync.peers().lock().await;

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
        for member in &members {
            if *member == my_id {
                continue;
            }

            if !sync_peers.contains(member) {
                app_log!("member {member} not connected, attempting to connect");
                let addr = NodeAddr::from(*member);
                self.sync.connect(self.router.endpoint(), addr).await?;
            }
        }

        Ok(())
    }
}
