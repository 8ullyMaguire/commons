//! Storage for Commons.
//!
//! One logical schema, two physical engines (plan §0.4): Postgres for a
//! federated index, SQLite for the local library. Everything above this crate
//! sees [`Store`], never a driver.
//!
//! Three things live here, and every later phase depends on them:
//!
//!   * the portable migration set, mirrored across both engines and held in
//!     step by `tests/migration_parity.rs`;
//!   * [`db::Store`], which picks the engine and applies migrations;
//!   * [`filter_ast`], the filter language that the UI, the shareable URL, and
//!     the federation protocol all speak.
//!
//! The consent rule that makes the rest of the design permissible is enforced
//! here rather than in the UI: [`Filter::consent_clause`] is derived from the
//! caller and there is no way to compile an object query without it.

pub mod bulk;
pub mod create;
pub mod db;
pub mod filter_ast;
pub mod folders;
// T-P6-003: the funscript row and its metadata. The actions are NOT
// stored -- see the module doc for why the path is the row and the timeline
// is recomputed per player load.
pub mod funscript;
pub mod fuzzy;
pub mod index;
pub mod locator;
pub mod media;
pub mod playback;
pub mod query;
pub mod relations;
pub mod search;
pub mod sort;
pub mod subtitles;
pub mod tags;
pub mod undo;

pub use db::{
    artifact_kinds, clear_jobs, file_path_and_state, file_rows, insert_artifact, insert_file,
    insert_job_if_absent, insert_object, job_by_dedupe_key, job_rows, mark_absent, set_path,
    update_file_content, update_job, Mode, NewFile, NewJob, Result, Store, StoreError, StoredFile,
    StoredJob,
};
pub use filter_ast::{
    base64url_decode, base64url_encode, BuiltinField, CallerId, CmpOp, ConsentTiers, Engine,
    FieldRef, Filter, FilterError, SqlFragment, Value,
};
pub use locator::{
    count as locator_count, destroy_for_object as destroy_locators, propose as propose_locator,
    ConsentFacts, LocatorScheme, ProposeError, ProposedLocator, StoredLocator,
};
