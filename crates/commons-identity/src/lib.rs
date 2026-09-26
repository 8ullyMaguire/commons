//! commons-identity — the identity engine (§7).
//!
//! Unsupervised clustering of face embeddings into `PersonCluster`s, with the
//! consent and ambiguity rules §7.1 requires. The design decision that shapes
//! this crate is written up in [`cluster`]: an engine that is too confident is
//! far more expensive than one that is too cautious, because every link it
//! makes is a claim about a real person and the user's trust in those claims is
//! the mechanism the whole product runs on.

pub mod cluster;

// §7.7's typed attributes and §7.9's status. Separate from `cluster` because a
// cluster is "these appearances are the same person" and a field is "this person
// has this value" -- the second needs the first to exist and says nothing about
// it.
pub mod attrs;

// §7.11's derived career span. Reads `object.date` through `appearance` and
// stores nothing, which is the whole design.
pub mod span;

// §7.5's self-service performer claim. Separate from `cluster` because it is
// about *people*: who is allowed to say which cluster is theirs, and what they
// may then change. The cluster engine has no opinion on any of that.
pub mod claim;
