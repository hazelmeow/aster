//! Authenticated causal broadcast implementation.

use crate::proto::clock::Clock;
use iroh_base::{NodeId, PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use std::{cmp::Reverse, collections::BinaryHeap};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message<P> {
    pub clock: Clock,
    pub payload: P,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedMessage<P> {
    pub inner: Message<P>,
    pub public_key: PublicKey,
    pub signature: Signature,
}

impl<P> SignedMessage<P> {
    pub fn seq(&self) -> u64 {
        self.inner.clock.get(self.public_key)
    }
}

/// Struct implementing authenticated causal broadcast.
///
/// TODO:
/// - message store
/// - message sync
#[derive(Debug)]
pub struct CausalBroadcast<P> {
    /// Secret key of the local node used for signing
    secret_key: SecretKey,
    /// Clock tracking delivered messages
    clock: Clock,
    /// All received messages, potentially undelivered
    received: Vec<SignedMessage<P>>,
}

impl<P: Clone + Serialize> CausalBroadcast<P> {
    pub fn new(secret_key: SecretKey) -> Self {
        Self {
            secret_key,
            clock: Clock::new(),
            received: Vec::new(),
        }
    }

    pub fn local_id(&self) -> NodeId {
        self.secret_key.public().into()
    }

    pub fn local_clock(&self) -> &Clock {
        &self.clock
    }

    // the message should have been handled locally already (will not be delivered to itself)
    pub fn make_message(&mut self, payload: P) -> SignedMessage<P> {
        self.clock.incr(self.local_id());

        let message = Message {
            clock: self.clock.clone(),
            payload,
        };

        // TODO: maybe message format for signing shouldn't be postcard even if wire format is postcard
        let message_bytes = postcard::to_stdvec(&message).expect("TODO");
        let signature = self.secret_key.sign(&message_bytes);

        let signed_message = SignedMessage {
            inner: message,
            public_key: self.secret_key.public(),
            signature,
        };

        self.received.push(signed_message.clone());

        signed_message
    }

    pub fn receive(&mut self, msg: &SignedMessage<P>) {
        // verify signature
        let message_bytes = postcard::to_stdvec(&msg.inner).expect("TODO");
        msg.public_key
            .verify(&message_bytes, &msg.signature)
            .expect("TODO. signature verification failed");

        // check if we already have the message
        // ie, check we have a message with the same seq # signed with the same public key
        if self
            .received
            .iter()
            .any(|m| m.public_key == msg.public_key && m.seq() == msg.seq())
        {
            return;
        }

        self.received.push(msg.clone());
    }

    fn can_deliver(&self, msg: &SignedMessage<P>) -> bool {
        // we can deliver a message with a given clock if:
        // - we have delivered the message preceding the message
        // - we have delivered all other causal predecessors of the message
        msg.inner.clock.iter().all(|(id, other_seq)| {
            let my_seq = self.clock.get(*id);

            if *id == msg.public_key {
                my_seq == other_seq - 1
            } else {
                my_seq >= *other_seq
            }
        })
    }

    pub fn deliver_next(&mut self) -> Option<&SignedMessage<P>> {
        for msg in &self.received {
            if self.can_deliver(msg) {
                self.clock.merge(&msg.inner.clock);
                return Some(msg);
            }
        }
        None
    }

    pub fn received(&self) -> impl Iterator<Item = &SignedMessage<P>> {
        self.received.iter()
    }

    pub fn delivered(&self) -> Vec<&SignedMessage<P>> {
        self.received
            .iter()
            .filter(|m| m.inner.clock.lte(&self.clock))
            .collect()
    }

    pub fn ordered(&self) -> Vec<&SignedMessage<P>> {
        let delivered = self.delivered();

        let n = delivered.len();

        // list of outgoing edges
        let mut edges = vec![Vec::new(); n];
        // number of incoming edges
        let mut incoming = vec![0; n];

        // build graph
        for (i, a) in delivered.iter().enumerate() {
            for (j, b) in delivered.iter().enumerate() {
                if i == j {
                    continue;
                }

                if a.inner.clock.happens_before(&b.inner.clock) {
                    edges[i].push(j);
                    incoming[j] += 1;
                }
            }
        }

        // find initial ready nodes
        let mut ready = BinaryHeap::new();
        for (i, &count) in incoming.iter().enumerate() {
            // check if no causal predecessors
            if count == 0 {
                // use Reverse since we want a min-heap, ordered by sender pk
                ready.push(Reverse((&delivered[i].public_key, i)))
            }
        }

        let mut result = Vec::with_capacity(n);

        // until done, consume ready nodes, using the binary heap ordering for tiebreaking
        while let Some(Reverse((_, i))) = ready.pop() {
            result.push(delivered[i]);

            // decrement children and check if ready
            for &j in &edges[i] {
                incoming[j] -= 1;
                if incoming[j] == 0 {
                    ready.push(Reverse((&delivered[j].public_key, j)));
                }
            }
        }

        result
    }
}

#[cfg(test)]
mod test {
    use arbtest::arbtest;
    use iroh_base::SecretKey;

    use super::CausalBroadcast;

    #[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
    struct Operation(u32);

    #[derive(Debug)]
    struct Actor {
        acb: CausalBroadcast<Operation>,
    }

    impl Actor {
        fn new(secret_key: SecretKey) -> Self {
            Self {
                acb: CausalBroadcast::new(secret_key),
            }
        }

        fn perform(&mut self, op: Operation) {
            self.acb.make_message(op);
        }
    }

    fn sync(alice: &mut Actor, bob: &mut Actor) {
        for msg in alice.acb.received() {
            bob.acb.receive(msg);
        }
        for msg in bob.acb.received() {
            alice.acb.receive(msg);
        }
    }

    #[test]
    fn same_final_order() {
        arbtest(|u| {
            let alice_sk = SecretKey::generate(&mut rand::rngs::OsRng);
            let bob_sk = SecretKey::generate(&mut rand::rngs::OsRng);
            let charlie_sk = SecretKey::generate(&mut rand::rngs::OsRng);

            let mut alice = Actor::new(alice_sk);
            let mut bob = Actor::new(bob_sk);
            let mut charlie = Actor::new(charlie_sk);

            let mut seq = 0;

            while !u.is_empty() {
                match *u.choose(&[
                    "alice_operation",
                    "bob_operation",
                    "charlie_operation",
                    "ab_sync",
                    "ac_sync",
                    "bc_sync",
                ])? {
                    "alice_operation" => {
                        println!("alice_operation");
                        alice.perform(Operation(rand::random()));
                        seq += 1;
                    }
                    "bob_operation" => {
                        println!("bob_operation");
                        bob.perform(Operation(rand::random()));
                        seq += 1;
                    }
                    "charlie_operation" => {
                        println!("charlie_operation");
                        charlie.perform(Operation(rand::random()));
                        seq += 1;
                    }
                    "ab_sync" => {
                        println!("ab_sync");
                        sync(&mut alice, &mut bob);
                        while let Some(next) = alice.acb.deliver_next() {}
                        while let Some(next) = bob.acb.deliver_next() {}
                    }
                    "ac_sync" => {
                        println!("ac_sync");
                        sync(&mut alice, &mut charlie);
                        while let Some(next) = alice.acb.deliver_next() {}
                        while let Some(next) = charlie.acb.deliver_next() {}
                    }
                    "bc_sync" => {
                        println!("bc_sync");
                        sync(&mut bob, &mut charlie);
                        while let Some(next) = bob.acb.deliver_next() {}
                        while let Some(next) = charlie.acb.deliver_next() {}
                    }
                    _ => unreachable!(),
                }
            }

            sync(&mut alice, &mut bob);
            sync(&mut alice, &mut charlie);
            sync(&mut bob, &mut charlie);

            while let Some(next) = alice.acb.deliver_next() {}
            while let Some(next) = bob.acb.deliver_next() {}
            while let Some(next) = charlie.acb.deliver_next() {}

            // should have delivered all messages
            assert!(alice.acb.delivered().len() == seq);
            assert!(bob.acb.delivered().len() == seq);
            assert!(charlie.acb.delivered().len() == seq);

            // should have same clocks
            assert!(alice.acb.local_clock().eq(bob.acb.local_clock()));
            assert!(alice.acb.local_clock().eq(charlie.acb.local_clock()));
            assert!(bob.acb.local_clock().eq(charlie.acb.local_clock()));

            // should be in the same order
            assert_eq!(alice.acb.ordered(), bob.acb.ordered());
            assert_eq!(alice.acb.ordered(), charlie.acb.ordered());
            assert_eq!(bob.acb.ordered(), charlie.acb.ordered());

            Ok(())
        });
    }
}
