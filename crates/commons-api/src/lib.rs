//! commons-api — see docs/spec/commons-spec.md and docs/plans/implementation-plan.md.
//!
//! # The shape of this crate
//!
//! HTTP surfaces over the domain crates. The rule it holds to is that
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
//!
//! # GraphQL is here now, hand-rolled, and that was measured (T-P6-007, T-P6-008)
//!
//! This doc used to say "HTTP and GraphQL surfaces". It held neither: the crate
//! was `pub mod claim;` over one 273-line file, while `async-graphql` and
//! `async-graphql-axum` sat declared in Cargo.toml and were called from no
//! `.rs` file in the workspace. T-P6-007 removed both declarations.
//!
//! T-P6-008 then measured whether they could be used, and the answer is no from
//! both ends: every `async-graphql-axum` from 7.0.17 up declares axum 0.8, so
//! its `GraphQLRequest` implements axum **0.8**'s `FromRequest` and cannot
//! satisfy this workspace's axum 0.7 `Handler` bound; and 7.0.0, the only 7.x
//! that declares axum 0.7, does not compile on this toolchain at all — 149
//! `E0195` errors inside the crate's own `model/directive.rs`. The 7.x line is
//! exhausted, so [`graphql`] is a hand-rolled resolver over `serde_json`.
//!
//! The cost is stated in [`graphql`]'s own module docs rather than here: no
//! introspection, no schema-driven validation, no field aliases. The UI sends
//! none of the three, which is what makes four operations viable.
//!
//! Note the two smaller claims this crate's history got wrong, both corrected
//! against the tree rather than deleted: the old doc said §11.5's GraphQL half
//! was "not built" because the UI's **eleven** GraphQL files are "proved
//! against a mock". Measured at `09176b8`, exactly **one** tracked `ui/` file
//! contains a GraphQL document and it declares **four** operations; the other
//! `gql` hits are Playwright fixtures in `ui/e2e/*` and `ui/tests/*`. And the
//! transport is swappable but its default is a real `fetch`, so the work was
//! never a client-wide conversion.
//!
//! Spec: `docs/spec/t-p6-007-public-api.md` §4, `docs/spec/t-p6-008-graphql.md`.
//!
//! # No policy in this crate
//!
//! [`graphql`] holds wire types and a dispatcher, and no rule. The consent gate
//! lives where it always has — in the store, behind the one sanctioned read —
//! and the resolver's only obligation is to pass the caller's `CallerId` into
//! it, which is what `caller_from_request` in `commons-server` is for.

/// §7.5's performer claim, over HTTP. See the module docs for the status-code
/// choices.
pub mod claim;

/// The GraphQL wire types and the operation-name dispatcher. T-P6-008.
pub mod graphql;
