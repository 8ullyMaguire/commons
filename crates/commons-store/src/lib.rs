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

pub mod db;
pub mod filter_ast;

pub use db::{
    artifact_kinds, clear_jobs, file_path_and_state, file_rows, insert_artifact, insert_file,
    insert_job_if_absent, insert_object, job_by_dedupe_key, job_rows, mark_absent, set_path,
    update_job, Mode, NewFile, NewJob, Result, Store, StoreError, StoredFile, StoredJob,
};
pub use filter_ast::{
    base64url_decode, base64url_encode, BuiltinField, CallerId, CmpOp, ConsentTiers, Engine,
    FieldRef, Filter, FilterError, SqlFragment, Value,
};
