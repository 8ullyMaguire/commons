//! commons-index — the curation engine (§8): field voting, candidate
//! generation, reputation, history and moderation.
//!
//! See docs/spec/commons-spec.md §8 and docs/plans/implementation-plan.md
//! Phase 4. `resolve` is the centre: every other module here either feeds it
//! proposals or reads what it settled.

pub mod reputation;
pub mod resolve;
