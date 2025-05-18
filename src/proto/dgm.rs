//! Decentralized group membership algorithm implementation.

use crate::proto::acb::CausalBroadcast;
use iroh_base::{NodeId, SecretKey};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Root(NodeId),
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
        Self {
            acb: CausalBroadcast::new(secret_key),
        }
    }

    pub fn evaluate_members(&self) -> HashSet<NodeId> {
        let mut members = HashSet::new();

        let mut ordered = self.acb.ordered();

        // TODO: bounds check
        let first = ordered.remove(0);

        match &first.inner.payload {
            Operation::Root(member) => {
                members.insert(*member);
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
                Operation::Root(_) => {
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
}
