//! commons-api — see docs/spec/commons-spec.md and docs/plans/implementation-plan.md.
//!
//! # The shape of this crate
//!
//! HTTP and GraphQL surfaces over the domain crates. The rule it holds to is that
//! **no policy lives here**: a handler parses a request, calls a domain crate,
//! and maps the domain's error type onto a status code. The reason is that the
//! rules are the part that has to be right, and a rule reachable only through one
//! router is a rule that gets re-implemented -- slightly differently -- when the
//! second router arrives (the CLI, the federation receiver, a maintenance tool).
//! Mapping errors is policy-adjacent, so it is the one thing here that is
//! deliberate, and [`claim::status_for`] documents why each mapping is what it is.
//!
//! The counterpart to that rule is that a domain error type must be exhaustively
//! mapped. A new variant added to a domain error will not compile here until
//! someone decides what a client should see, which is the point: the decision is
//! unavoidable, and making it at the API boundary is where it belongs.

//! §7.5's performer claim, over HTTP. See the module docs for the status-code
//! choices.
pub mod claim;
