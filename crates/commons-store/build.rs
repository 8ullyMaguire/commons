//! Make cargo re-run when a migration file is added or changed.
//!
//! # The problem this exists to solve
//!
//! `sqlx::migrate!("./migrations/sqlite")` embeds the migrations at compile
//! time, which is the right design -- a binary cannot be run against a schema it
//! does not carry. But the macro's dependency tracking covers the files it
//! already found, and a *newly added* migration does not invalidate the
//! compiled-in set.
//!
//! The symptom is a build that is green and a test that cannot see the table it
//! just wrote a migration for:
//!
//! ```text
//! SqliteError { code: 1, message: "no such table: performer_claim" }
//! ```
//!
//! with a migration file sitting in `migrations/postgres/` and its mirror in
//! `migrations/sqlite/`. It cost two debugging sessions on T-P3-003 and
//! T-P3-005, both times on the same crate, and the second time I had already
//! written the lesson down and still hit it -- which is the sign that the fix
//! belongs in the build rather than in a note.
//!
//! # Why a directory, not the files
//!
//! `cargo:rerun-if-changed` on each file would need this build script to know
//! the file list, and it is precisely the *new* file it cannot know about. The
//! directory is the one path whose change implies a rebuild, and the cost of a
//! spurious rebuild is nil.

use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for engine in ["sqlite", "postgres"] {
        println!(
            "cargo:rerun-if-changed={}",
            root.join("migrations").join(engine).display()
        );
    }
    println!("cargo:rerun-if-changed=build.rs");
}
