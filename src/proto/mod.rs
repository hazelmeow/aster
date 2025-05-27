//! Protocol logic.

mod acb;
mod clock;
mod dgm;
pub(crate) mod join;
pub(crate) mod sync;

use crate::{
    profile::Profile,
    proto::{join::JoinProtocol, sync::SyncProtocol},
};
use base64::{Engine, prelude::BASE64_STANDARD_NO_PAD};
use dgm::GroupMembership;
use iroh::{NodeAddr, NodeId, protocol::Router};
use std::{collections::HashSet, sync::Arc};
use tokio::sync::Mutex;

/// View of protocol state for UI.
///
/// Polled periodically since we can't lock the state mutices during rendering.
#[derive(Debug, Clone, Default)]
pub struct ProtocolState {
    pub peers: Vec<NodeId>,
    pub join_code: String,
    pub group_id: Option<u64>,
    pub group_members: Option<HashSet<NodeId>>,
}

#[derive(Debug)]

pub struct Protocol {
    pub router: Router,
    sync: SyncProtocol,
    join: JoinProtocol,

    group: Mutex<Option<GroupMembership>>,
}

impl Protocol {
    pub async fn new(profile: &Profile) -> anyhow::Result<Self> {
        // create iroh endpoint
        let endpoint = iroh::Endpoint::builder()
            .discovery_n0()
            .secret_key(profile.secret_key().clone())
            .bind()
            .await?;

        let sync = SyncProtocol::new();
        let join = JoinProtocol::new();

        let router = iroh::protocol::Router::builder(endpoint)
            .accept(SyncProtocol::ALPN, sync.clone())
            .accept(JoinProtocol::ALPN, join.clone())
            .spawn()
            .await?;

        Ok(Self {
            router,
            sync,
            join,

            group: Mutex::new(None),
        })
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

        let join_code = {
            let node_id = self.router.endpoint().node_id();
            let code = self.join.get_code().await;

            let mut bytes = [0u8; 40];
            bytes[0..32].copy_from_slice(node_id.as_bytes());
            bytes[32..40].copy_from_slice(&code.to_be_bytes());

            BASE64_STANDARD_NO_PAD.encode(&bytes)
        };

        let (group_id, group_members) = {
            let group = self.group.lock().await;
            let group_state = group
                .as_ref()
                .map(|dgm| (dgm.group_id(), dgm.evaluate_members()));
            (group_state.as_ref().map(|s| s.0), group_state.map(|s| s.1))
        };

        Ok(ProtocolState {
            peers,
            join_code,
            group_id,
            group_members,
        })
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
        let mut group = self.group.lock().await;

        let group = group.as_mut().ok_or(anyhow::anyhow!("not in a group"))?;
        group.add_member(node_id);

        Ok(())
    }

    pub async fn join_node(&self, addr: NodeAddr, code: u64) -> anyhow::Result<()> {
        self.join.join(self.router.endpoint(), addr, code).await?;
        Ok(())
    }
}
