//! commons-identity — the identity engine (§7).
//!
//! Unsupervised clustering of face embeddings into `PersonCluster`s, with the
//! consent and ambiguity rules §7.1 requires. The design decision that shapes
//! this crate is written up in [`cluster`]: an engine that is too confident is
//! far more expensive than one that is too cautious, because every link it
//! makes is a claim about a real person and the user's trust in those claims is
//! the mechanism the whole product runs on.

pub mod cluster;
