//! Decentralized group membership algorithm implementation.

use crate::proto::acb::{CausalBroadcast, SignedMessage};
use iroh_base::{NodeId, SecretKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Root {
        group_id: u64, // TODO
        root_member: NodeId,
    },
    Add(NodeId),
    Remove(NodeId),
}

/// Struct implementing decentralized group membership.
#[derive(Debug)]
pub struct GroupMembership {
    pub acb: CausalBroadcast<Operation>,
}

impl GroupMembership {
    pub fn new(secret_key: SecretKey) -> Self {
        let group_id = rand::random();
        let root = Operation::Root {
            group_id,
            root_member: secret_key.public(),
        };

        let mut acb = CausalBroadcast::new(secret_key);
        acb.make_message(root);

        Self { acb }
    }
    pub fn from_causal_broadcast(acb: CausalBroadcast<Operation>) -> Self {
        // TODO: verify structure

        Self { acb }
    }

    pub fn add_member(&mut self, node_id: NodeId) -> SignedMessage<Operation> {
        self.acb.make_message(Operation::Add(node_id))
    }

    pub fn remove_member(&mut self, node_id: NodeId) -> SignedMessage<Operation> {
        self.acb.make_message(Operation::Remove(node_id))
    }

    pub fn evaluate_members(&self) -> HashSet<NodeId> {
        let mut members = HashSet::new();

        let mut ordered = self.acb.ordered();

        // TODO: bounds check
        let first = ordered.remove(0);

        match &first.inner.payload {
            Operation::Root {
                group_id,
                root_member,
            } => {
                members.insert(*root_member);
            }
            _ => {
                panic!("must start with root");
            }
        }

        for msg in ordered {
            // check that message is from grup member
            if !members.contains(&msg.public_key) {
                continue;
            }

            match &msg.inner.payload {
                Operation::Root { .. } => {
                    panic!("must not contain additional roots");
                }
                Operation::Add(member) => {
                    members.insert(*member);
                }
                Operation::Remove(member) => {
                    members.remove(member);
                }
            }
        }

        members
    }

    // TODO
    pub fn group_id(&self) -> u64 {
        let ordered = self.acb.ordered();
        assert!(ordered.len() >= 1);
        let root = ordered.iter().find_map(|m| match m.inner.payload {
            Operation::Root { group_id, .. } => Some(group_id),
            _ => None,
        });
        root.unwrap()
    }
}
