//! Vector clock implementation.

use iroh_base::NodeId;
use serde::{Deserialize, Serialize};

/// Vector clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    inner: Vec<(NodeId, u64)>,
}

impl Clock {
    pub fn new() -> Self {
        Self { inner: Vec::new() }
    }

    pub fn incr(&mut self, node_id: NodeId) -> u64 {
        match self.inner.iter_mut().find(|(id, _)| *id == node_id) {
            Some((_, seq)) => {
                *seq += 1;
                *seq
            }
            None => {
                self.inner.push((node_id, 1));
                1
            }
        }
    }

    pub fn get(&self, node_id: NodeId) -> u64 {
        match self.inner.iter().find(|(id, _)| *id == node_id) {
            Some((_, seq)) => *seq,
            None => 0,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(NodeId, u64)> {
        self.inner.iter()
    }

    // returns (some_lt, some_gt)
    // ie some of our entries are less than their entries
    // some of our entries are greater than their entries
    fn compare(&self, other: &Self) -> (bool, bool) {
        let mut some_lt = false;
        let mut some_gt = false;

        for (id, other_seq) in other.inner.iter() {
            let my_seq = self.get(*id);
            if my_seq < *other_seq {
                some_lt = true;
            } else if my_seq > *other_seq {
                some_gt = true;
            }
        }
        for (id, my_seq) in self.inner.iter() {
            let other_seq = other.get(*id);
            if *my_seq < other_seq {
                some_lt = true;
            } else if *my_seq > other_seq {
                some_gt = true;
            }
        }

        (some_lt, some_gt)
    }

    pub fn eq(&self, other: &Self) -> bool {
        let (some_lt, some_gt) = self.compare(other);
        !some_lt && !some_gt
    }

    pub fn happens_before(&self, other: &Self) -> bool {
        let (some_lt, some_gt) = self.compare(other);
        some_lt && !some_gt
    }

    pub fn happens_after(&self, other: &Self) -> bool {
        let (some_lt, some_gt) = self.compare(other);
        !some_lt && some_gt
    }

    pub fn concurrent_with(&self, other: &Self) -> bool {
        let (some_lt, some_gt) = self.compare(other);
        some_lt && some_gt
    }

    pub fn lte(&self, other: &Self) -> bool {
        let (some_lt, some_gt) = self.compare(other);
        !some_gt
    }

    pub fn merge(&mut self, other: &Self) {
        for (id, other_seq) in other.inner.iter() {
            match self.inner.iter_mut().find(|(i, _)| i == id) {
                Some((_, seq)) => {
                    if other_seq > seq {
                        *seq = *other_seq;
                    }
                }
                None => {
                    self.inner.push((id.to_owned(), *other_seq));
                }
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::Clock;
    use iroh_base::SecretKey;

    #[test]
    pub fn incr() {
        let alice = SecretKey::generate(&mut rand::rngs::OsRng).public().into();

        let mut clock = Clock::new();

        assert_eq!(clock.get(alice), 0);

        clock.incr(alice);

        assert_eq!(clock.get(alice), 1);
    }

    #[test]
    pub fn eq() {
        let alice = SecretKey::generate(&mut rand::rngs::OsRng).public().into();

        let mut t1 = Clock::new();
        let mut t2 = Clock::new();

        assert!(t1.eq(&t2));
        assert!(t2.eq(&t1));

        t1.incr(alice);

        assert!(!t1.eq(&t2));
        assert!(!t2.eq(&t1));

        t2.incr(alice);

        assert!(t1.eq(&t2));
        assert!(t2.eq(&t1));
    }

    #[test]
    pub fn one_value() {
        let alice = SecretKey::generate(&mut rand::rngs::OsRng).public().into();

        let mut t1 = Clock::new();
        let t2 = Clock::new();

        assert!(t1.eq(&t2));
        assert!(t2.eq(&t1));
        assert!(!t1.happens_before(&t2));
        assert!(!t2.happens_before(&t1));
        assert!(!t1.happens_after(&t2));
        assert!(!t2.happens_after(&t1));
        assert!(!t1.concurrent_with(&t2));
        assert!(!t2.concurrent_with(&t1));

        t1.incr(alice);

        assert!(!t1.eq(&t2));
        assert!(!t2.eq(&t1));
        assert!(!t1.happens_before(&t2));
        assert!(t2.happens_before(&t1));
        assert!(t1.happens_after(&t2));
        assert!(!t2.happens_after(&t1));
        assert!(!t1.concurrent_with(&t2));
        assert!(!t2.concurrent_with(&t1));
    }

    #[test]
    pub fn concurrent() {
        let alice = SecretKey::generate(&mut rand::rngs::OsRng).public().into();
        let bob = SecretKey::generate(&mut rand::rngs::OsRng).public().into();

        let mut t1 = Clock::new();
        let mut t2 = Clock::new();

        assert!(t1.eq(&t2));
        assert!(t2.eq(&t1));
        assert!(!t1.happens_before(&t2));
        assert!(!t2.happens_before(&t1));
        assert!(!t1.happens_after(&t2));
        assert!(!t2.happens_after(&t1));
        assert!(!t1.concurrent_with(&t2));
        assert!(!t2.concurrent_with(&t1));

        t1.incr(alice);
        t2.incr(bob);

        assert!(!t1.eq(&t2));
        assert!(!t2.eq(&t1));
        assert!(!t1.happens_before(&t2));
        assert!(!t2.happens_before(&t1));
        assert!(!t1.happens_after(&t2));
        assert!(!t2.happens_after(&t1));
        assert!(t1.concurrent_with(&t2));
        assert!(t2.concurrent_with(&t1));
    }

    #[test]
    pub fn merge() {
        let alice = SecretKey::generate(&mut rand::rngs::OsRng).public().into();

        let mut t1 = Clock::new();
        let mut t2 = Clock::new();

        t1.incr(alice);

        assert_eq!(t2.get(alice), 0);
        t2.merge(&t1);
        assert_eq!(t2.get(alice), 1);
    }
}
