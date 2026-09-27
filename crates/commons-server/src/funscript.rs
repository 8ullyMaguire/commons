//! Funscript routes: the list, and the timeline (T-P6-003, spec §5.6).
//!
//! # Two routes, and why they are two
//!
//! `GET /media/:object_id/funscripts` returns the rows — paths, axis counts,
//! metadata — and nothing else. It is cheap, it changes when a re-scan finds
//! something, and it is what a library view needs.
//!
//! `GET /media/:object_id/funscripts/:id` returns the **sampled timeline**:
//! every axis, every action, as JSON. It is the expensive one, it is only
//! wanted when a player is actually opening, and it is a different failure
//! mode from the list — a list can succeed while every script on it is
//! unreadable, and a player that cannot tell those apart shows an empty
//! timeline and the user concludes their funscript is broken.
//!
//! # This is the first route that serves a path FROM the database
//!
//! Every other media route opens `MediaLocation::path`, which the index
//! recorded from a walk of the library root. A funscript's path is recorded
//! the same way, **or supplied by a plugin** — `FunscriptSource::Provided` is a
//! real variant, and a plugin is code a user installed. So this route reads a
//! path out of a column and opens it, and that makes containment the load-bearing
//! check in the file.
//!
//! [`contained_in`] is that check, it is a pure function, and it is tested
//! against the traversal spellings rather than trusted. The reasoning:
//!
//! - **Lexical normalisation is not enough on its own.** `a/../../etc/passwd`
//!   normalises to `../etc/passwd` and is obviously out. But a **symlink**
//!   inside the root can point anywhere and survives lexical normalisation
//!   completely, so the check canonicalises and then compares the *real* paths.
//! - **Comparing canonicalised paths is not enough either**, because
//!   canonicalising a path that does not exist returns the input unchanged in
//!   some cases and errors in others. So the file must exist for the check to
//!   mean anything, and a path that does not exist is a 404 — which is also the
//!   right answer independently, since there are no bytes to serve.
//! - **The root itself must be canonicalised too**, or a symlinked library
//!   directory makes every legitimate path look like it is outside itself.
//!
//! The failure is deliberately a **404, not a 403**. A 403 confirms the script
//! exists, which is the leak the gate exists to prevent — the same reasoning as
//! the subtitle routes, and the same gate: `store.media_path`, so there is one
//! place that decides whether a caller may see an object's files at all.
//!
//! # The response is the timeline, not the file
//!
//! A funscript is JSON that Commons did not write, and a player needs typed
//! actions with interpolated positions — not the file. Serving the raw file
//! would push the parsing into the browser, which is where the lenient reading
//! of a malformed script would then have to live, and a lenient parser in
//! TypeScript and a strict one in Rust disagreeing about the same file is a bug
//! that only reproduces on one machine.

use axum::extract::{Path as AxumPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use commons_scan::funscript::Action;
use commons_scan::funscript_timeline::{Interpolation, Timeline};
use commons_store::funscript as store_fs;
use serde::Serialize;

use crate::media::{local_caller, not_found};

use crate::AppState;

/// One funscript in the list, as a player needs it.
///
/// Not the row. The row carries `created_at` and the internal id, which are
/// for the scanner and for debugging. What is kept is the three things a
/// client cannot work out for itself: how many axes, where the file is, and
/// what the script says about itself.
#[derive(Debug, Serialize)]
pub struct FunscriptSummary {
    pub id: String,
    pub path: String,
    pub axis_count: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<store_fs::FunscriptMetadata>,
}

/// One action on the wire.
///
/// Deliberately the script's own `at`/`pos` pair and nothing else. The
/// `next_*`/`exact` fields on [`Sample`] belong to a per-frame READ, and
/// putting them on every stored action would suggest a caller needs them --
/// there is no "next" for the last action in a list, and every action in a
/// list is by definition exact. The player computes those as it plays, from
/// two actions it already has.
#[derive(Debug, Serialize)]
pub struct ActionWire {
    pub at_ms: u64,
    pub position: f32,
}

impl From<Action> for ActionWire {
    fn from(a: Action) -> Self {
        Self {
            at_ms: a.at_ms,
            position: a.position,
        }
    }
}

/// One axis's actions, pre-sampled.
#[derive(Debug, Serialize)]
pub struct AxisWire {
    pub name: String,
    pub actions: Vec<ActionWire>,
}

/// The whole timeline, as the player receives it.
#[derive(Debug, Serialize)]
pub struct TimelineWire {
    pub axes: Vec<AxisWire>,
    pub source: String,
    /// What the parser had to do to make sense of the file, carried to the
    /// browser so the player can TELL the user rather than silently handing
    /// them a short timeline. A user whose script lost 40 actions to a
    /// malformed entry needs to be told, or they will think the file is wrong.
    pub warnings: Vec<String>,
    pub span_ms: u64,
}

/// `GET /media/:object_id/funscripts`.
///
/// The gate is the same `media_path` call `/media/:id` makes, so a caller that
/// cannot see the video cannot enumerate its scripts. A **200 with an empty
/// list** would be the leak — the caller cannot tell "no scripts" from "not
/// allowed to know" — so absent and denied both answer 404.
pub async fn list(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath(object_id): AxumPath<String>,
) -> Response {
    let caller = local_caller();
    match state.store.media_path(&object_id, &caller).await {
        Ok(Some(_)) => {}
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "funscripts: media_path failed");
            return internal_error();
        }
    }

    let rows = match store_fs::list_for_object(&state.store, &object_id).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "funscripts: list failed");
            return internal_error();
        }
    };

    let body: Vec<FunscriptSummary> = rows
        .into_iter()
        .map(|r| {
            // Read BEFORE the fields are moved. `r.metadata()` borrows `r` to
            // decode the column, and doing it after `r.id` is moved is a
            // partial move of a value still in use -- which the compiler
            // catches, and which is a hint the order was chosen carelessly.
            let metadata = r.metadata();
            FunscriptSummary {
                id: r.id,
                path: r.path,
                axis_count: r.axis_count,
                // Decoded here rather than shipped as a string: the player
                // wants fields, and a client parsing a JSON string inside a
                // JSON response is a parser in every browser for no reason. A
                // corrupt blob reads as None rather than failing -- see the
                // store module.
                metadata,
            }
        })
        .collect();

    json(StatusCode::OK, &body)
}

/// `GET /media/:object_id/funscripts/:funscript_id`.
///
/// `?interpolation=linear` selects the reading; the default is `step`, which
/// is the format's own. `?duration_ms=N` clamps the view to N milliseconds
/// **without touching the stored script** — a zero or absent value means "not
/// asked for", which is not the same as zero-length and must not wipe the
/// timeline.
pub async fn timeline(
    State(state): State<std::sync::Arc<AppState>>,
    AxumPath((object_id, funscript_id)): AxumPath<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<TimelineQuery>,
) -> Response {
    // Same gate, same reason as every other route in this file -- and this is
    // where the video's DIRECTORY comes from, which is the anchor the
    // containment check needs. The gate is load-bearing twice over: it decides
    // whether the caller may see the object at all, and it is the only
    // requester-independent statement of where this object's files live.
    let video_dir = match state.store.media_path(&object_id, &local_caller()).await {
        Ok(Some(loc)) => loc.path.parent().map(std::path::Path::to_path_buf),
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(object_id = %object_id, error = %e, "funscripts: media_path failed");
            return internal_error();
        }
    };
    let Some(video_dir) = video_dir else {
        // A media path with no parent is a relative bare filename, which the
        // index does not produce. Refusing is the safe reading: without a
        // directory there is no anchor, and a check with no anchor is no check.
        return not_found();
    };

    let row = match store_fs::get(&state.store, &funscript_id).await {
        Ok(Some(r)) => r,
        Ok(None) => return not_found(),
        Err(e) => {
            tracing::error!(funscript_id = %funscript_id, error = %e, "funscripts: get failed");
            return internal_error();
        }
    };

    // The ownership check, and it is a string comparison on purpose: the id in
    // the URL is caller-supplied, so the question is "is this row this
    // object's", and anything looser serves one object's script under another's
    // id. 404 rather than 403, for the reason in the module doc.
    if row.object_id != object_id {
        return not_found();
    }

    let text = match read_contained(&video_dir, &row.path).await {
        Ok(t) => t,
        Err(ReadError::NotFound) => return not_found(),
        Err(ReadError::OutsideRoot) => {
            // Logged, not returned. A recorded path outside the library root is
            // either a bug in the scanner or a plugin supplying one on purpose,
            // and an operator needs to see which -- but the caller is told
            // nothing beyond the 404.
            tracing::warn!(
                object_id = %object_id,
                funscript_id = %funscript_id,
                path = %row.path,
                "funscript path is outside the library root; refusing"
            );
            return not_found();
        }
        Err(ReadError::Io(e)) => {
            tracing::error!(path = %row.path, error = %e, "funscript read failed");
            return internal_error();
        }
    };

    let script = match commons_scan::funscript::Funscript::parse(&text) {
        Ok(s) => s,
        Err(e) => {
            // A file that exists and will not parse is a 422, not a 404: the
            // script IS there, and the user needs to be told their file is
            // unreadable rather than that it is missing. Collapsing the two
            // sends them looking for a file that is sitting right there.
            tracing::warn!(path = %row.path, error = %e, "funscript did not parse");
            return json(
                StatusCode::UNPROCESSABLE_ENTITY,
                &serde_json::json!({
                    "error": "funscript_unreadable",
                    "detail": e.to_string(),
                }),
            );
        }
    };

    let tl = Timeline::new(&script, q.interpolation(), q.clamped_duration());

    // The TIMELINE, not a sample of it. `Timeline::sample_all(t)` answers "where
    // is every axis at instant t", which is a per-frame question; a player
    // needs the whole action list so it can seek. Shipping `sample_all(duration)`
    // -- which is what this did first -- returns ONE position per axis and a
    // 20,000-action script arrives as 1, so the player seeks to a constant.
    let duration = q.clamped_duration();
    let body = TimelineWire {
        axes: tl
            .axis_names()
            .iter()
            .enumerate()
            .map(|(i, name)| AxisWire {
                name: (*name).to_string(),
                actions: tl
                    .axis_actions(i, duration)
                    .into_iter()
                    .map(ActionWire::from)
                    .collect(),
            })
            .collect(),
        source: tl.source.to_string(),
        warnings: tl.warnings.clone(),
        span_ms: tl.span_ms(),
    };

    // A script with no usable axes is a 200 with empty axes and the warnings
    // that explain it, NOT a 404. "The file parsed and has nothing in it" and
    // "there is no file" are different facts and the parser is the thing that
    // knows which.
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_string(&body).unwrap_or_else(|e| {
            tracing::error!(error = %e, "funscript serialise failed");
            r#"{"error":"funscript_serialise_failed"}"#.to_string()
        }),
    )
        .into_response()
}

/// The query parameters `timeline` accepts.
#[derive(Debug, Default, serde::Deserialize)]
pub struct TimelineQuery {
    /// `step` (the default) or `linear`. An unknown value is `step`, not an
    /// error: a client sending a value from a newer Commons should still get a
    /// timeline, and `step` is the safe reading to fall back to.
    pub interpolation: Option<String>,
    /// Clamp the view. Absent or zero means "the whole script" — see the
    /// module note on why zero is not a length.
    pub duration_ms: Option<u64>,
}

impl TimelineQuery {
    fn interpolation(&self) -> Interpolation {
        match self.interpolation.as_deref() {
            // Case-insensitive, because a rejected value silently falls back
            // to `step` -- and a client that sent "Linear" and got `step` sees
            // the feature not working rather than sees a typo.
            Some(v) if v.eq_ignore_ascii_case("linear") => Interpolation::Linear,
            // Everything else, including a typo, is `step`.
            _ => Interpolation::Step,
        }
    }

    fn clamped_duration(&self) -> u64 {
        self.duration_ms.unwrap_or(0)
    }
}

// ---------------------------------------------------------------------------
// Reading a path out of the database
// ---------------------------------------------------------------------------

/// Why a recorded path could not be read.
#[derive(Debug)]
enum ReadError {
    /// No such file, or it is not a file. A 404.
    NotFound,
    /// The path resolves outside the library root. A 404, and a log line.
    OutsideRoot,
    Io(std::io::Error),
}

/// Read a funscript, refusing anything that resolves outside the video's own
/// directory.
///
/// # Why the anchor is the VIDEO's directory and not the script's
///
/// The first version took the script file's own parent as the root, and that
/// check is **worthless**: `parent/../anything` has the script's parent as an
/// ancestor of its own canonical form, so a path that walks out is measured
/// against a root it is already outside of, and the test that proved it
/// wrong failed with `expected OutsideRoot, got true`.
///
/// The anchor has to be something the caller cannot influence by choosing a
/// path — and the only such thing on this request is the **video's** directory,
/// which came from `media_path`, the call the consent gate already made. A
/// funscript is by definition beside its video or in a `*.funscript/`
/// directory beside it, so "inside the video's directory, or inside a
/// subdirectory of it" is exactly the property that must hold, and a `..`
/// breaks it because it leaves the video's directory rather than a
/// directory the requester chose.
///
/// A `*.funscript/` directory is a SUBDIRECTORY of the video's, so
/// `starts_with` admits it without a special case.
async fn read_contained(video_dir: &std::path::Path, path: &str) -> Result<String, ReadError> {
    let p = std::path::Path::new(path);

    let root = match video_dir.canonicalize() {
        Ok(r) => r,
        // The video's own directory does not exist. That is an index state the
        // gate already tolerates, and there are no bytes to serve either way.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ReadError::NotFound),
        Err(e) => return Err(ReadError::Io(e)),
    };

    let real = match p.canonicalize() {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(ReadError::NotFound),
        Err(e) => return Err(ReadError::Io(e)),
    };

    if !contained_in(&real, &root) {
        return Err(ReadError::OutsideRoot);
    }

    tokio::fs::read_to_string(&real)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ReadError::NotFound,
            _ => ReadError::Io(e),
        })
}

/// Whether `candidate` is `root` or lives inside it.
///
/// Three rules, and each one exists because a plausible simplification of it is
/// wrong:
///
/// - **Canonicalise BOTH sides.** Comparing a canonicalised candidate against a
///   root that was not canonicalised makes a symlinked library directory
///   reject every legitimate file in it, because `/data` and `/var/lib/commons`
///   are the same directory under two names.
/// - **Compare components, not string prefixes.** `starts_with` on a `Path`
///   compares components, but a *string* prefix does not: `/library-evil` starts
///   with `/library`. This takes `PathBuf`s and uses `Path::starts_with`, which
///   is component-wise, and says so because the string version is the mistake
///   this function exists to prevent.
/// - **A path equal to the root counts as contained**, which is what
///   `Path::starts_with` already does and is worth stating because the obvious
///   hand-rolled loop gets it wrong.
pub fn contained_in(candidate: &std::path::Path, root: &std::path::Path) -> bool {
    // `Path::starts_with` compares whole components, so `/lib` does not match
    // `/library`. That is the property a string prefix gets wrong.
    candidate.starts_with(root)
}

fn json<T: Serialize>(status: StatusCode, body: &T) -> Response {
    match serde_json::to_string(body) {
        Ok(s) => (status, [(header::CONTENT_TYPE, "application/json")], s).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "funscript serialise failed");
            internal_error()
        }
    }
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"error":"funscript_lookup_failed"}"#,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_inside_the_root_is_contained() {
        assert!(contained_in(
            std::path::Path::new("/library/clip.mp4.funscript"),
            std::path::Path::new("/library"),
        ));
    }

    /// The string-prefix mistake, stated as a test. `/library-evil` shares a
    /// string prefix with `/library` and is a different directory, and a
    /// `starts_with` on the rendered path would wave it through.
    #[test]
    fn a_sibling_directory_with_a_shared_string_prefix_is_not_contained() {
        assert!(
            !contained_in(
                std::path::Path::new("/library-evil/clip.funscript"),
                std::path::Path::new("/library"),
            ),
            "a string prefix would have let this through"
        );
    }

    /// `contained_in` is LEXICAL, and this test is the statement of what that
    /// means and does not mean.
    ///
    /// `/library/../etc/passwd` is component-wise *inside* `/library`, and this
    /// function says yes -- correctly, because its contract is "given two
    /// canonical paths, is the first inside the second", and the caller
    /// canonicalises before asking. The function that catches a traversal is
    /// `canonicalize`, and the test that proves it is
    /// `reading_a_traversal_refuses_it`, which goes through the real read path.
    ///
    /// Written the other way -- asserting here that a `..` string is refused --
    /// the test would have been testing `Path::components` rather than the
    /// security property, and it would have passed against an implementation
    /// whose callers forgot to canonicalise.
    #[test]
    fn a_lexical_check_alone_does_not_see_a_traversal() {
        assert!(
            contained_in(
                std::path::Path::new("/library/../etc/passwd"),
                std::path::Path::new("/library")
            ),
            "lexically it IS inside -- which is why the caller canonicalises"
        );
    }

    /// What `contained_in` does catch on its own: a path that is outright
    /// outside, with no `..` needed.
    #[test]
    fn a_path_outright_outside_the_root_is_not_contained() {
        for p in ["/etc/passwd", "/library-evil/a.funscript", "/other/a"] {
            assert!(
                !contained_in(std::path::Path::new(p), std::path::Path::new("/library")),
                "{p} escaped"
            );
        }
    }

    #[test]
    fn the_root_itself_is_contained() {
        // `Path::starts_with` says yes, and a hand-rolled component loop that
        // required a longer path would say no.
        assert!(contained_in(
            std::path::Path::new("/library"),
            std::path::Path::new("/library"),
        ));
    }

    /// The case that makes lexical checks insufficient: a symlink inside the
    /// root pointing out of it. Nothing about the path is wrong; only the
    /// canonical form shows where it lands.
    #[test]
    fn a_symlink_out_of_the_root_is_refused_after_canonicalisation() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.funscript");
        std::fs::write(&secret, r#"{"actions":[]}"#).unwrap();

        let link = dir.path().join("clip.mp4.funscript");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        // Lexically it is inside.
        assert!(contained_in(&link, dir.path()));
        // Canonically it is not, which is the check that matters.
        let real = link.canonicalize().unwrap();
        let root = dir.path().canonicalize().unwrap();
        #[cfg(unix)]
        assert!(
            !contained_in(&real, &root),
            "a symlink out of the root must be refused"
        );
    }

    /// Reading through the real function, so the containment check is proven
    /// wired rather than merely present next to the read.
    #[tokio::test]
    async fn reading_a_traversal_refuses_it() {
        // The video lives in `dir`; the script claims to live beside it but
        // walks out to a directory that does not. This is the case the first
        // version got wrong, and it got wrong it because the ROOT was the
        // script's own parent -- which a `..` leaves.
        let video_dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.funscript");
        std::fs::write(&secret, r#"{"actions":[]}"#).unwrap();
        let real_video = video_dir.path().join("clip.mp4");
        std::fs::write(&real_video, b"x").unwrap();

        let escape = format!(
            "{}/../{}/secret.funscript",
            video_dir.path().display(),
            outside.path().file_name().unwrap().to_string_lossy()
        );
        assert!(
            matches!(
                read_contained(video_dir.path(), &escape).await,
                Err(ReadError::OutsideRoot)
            ),
            "a path that leaves the video's directory must be refused"
        );
    }

    /// A script in a `*.funscript/` DIRECTORY beside the video is the multi-axis
    /// form, and it is a subdirectory of the video's -- so containment must
    /// admit it without a special case.
    #[tokio::test]
    async fn a_funscript_directory_beside_the_video_is_inside_the_root() {
        let video_dir = tempfile::tempdir().unwrap();
        let sub = video_dir.path().join("clip.funscript");
        std::fs::create_dir(&sub).unwrap();
        let p = sub.join("a.json");
        std::fs::write(&p, r#"{"axes":[{"name":"stroke","actions":[]}]}"#).unwrap();
        let text = read_contained(video_dir.path(), &p.to_string_lossy())
            .await
            .expect("a subdirectory of the video's directory is inside it");
        assert!(text.contains("stroke"), "{text}");
    }

    /// The same script placed OUTSIDE the video's directory is refused, even
    /// though it parses and even though the caller was allowed to see the
    /// object. Containment is about the path, not about the consent gate.
    #[tokio::test]
    async fn a_valid_script_outside_the_video_directory_is_still_refused() {
        let video_dir = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let p = elsewhere.path().join("a.funscript");
        std::fs::write(&p, r#"{"actions":[{"at":0,"pos":0.5}]}"#).unwrap();
        assert!(matches!(
            read_contained(video_dir.path(), &p.to_string_lossy()).await,
            Err(ReadError::OutsideRoot)
        ));
    }

    #[tokio::test]
    async fn reading_a_real_sidecar_returns_its_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("clip.mp4.funscript");
        std::fs::write(&p, r#"{"actions":[{"at":0,"pos":0.5}]}"#).unwrap();
        let text = read_contained(dir.path(), &p.to_string_lossy())
            .await
            .expect("it is right there");
        assert!(text.contains("\"pos\":0.5"), "{text}");
    }

    #[tokio::test]
    async fn reading_a_missing_file_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("gone.funscript");
        assert!(matches!(
            read_contained(dir.path(), &p.to_string_lossy()).await,
            Err(ReadError::NotFound)
        ));
    }

    #[test]
    fn a_symlinked_library_directory_does_not_reject_its_own_files() {
        // The mirror of the symlink-escape test: if the ROOT is not
        // canonicalised either, a library reached through a symlink rejects
        // every file in it.
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real-library");
        std::fs::create_dir(&real).unwrap();
        let f = real.join("clip.mp4.funscript");
        std::fs::write(&f, r#"{"actions":[]}"#).unwrap();

        #[cfg(unix)]
        {
            let link = dir.path().join("linked-library");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let root = link.canonicalize().unwrap();
            let candidate = f.canonicalize().unwrap();
            assert!(
                contained_in(&candidate, &root),
                "a file reached through the real path must be inside the \
                 canonicalised root"
            );
        }
    }

    // ---- the query defaults, which are decisions ----

    #[test]
    fn an_absent_interpolation_is_step() {
        assert_eq!(
            TimelineQuery::default().interpolation(),
            Interpolation::Step
        );
    }

    /// A client from a newer Commons sending a value this one does not know
    /// still gets a timeline, and gets the format's own reading rather than an
    /// error.
    #[test]
    fn an_unknown_interpolation_falls_back_to_step_rather_than_failing() {
        let q = TimelineQuery {
            interpolation: Some("cubic-hermite-from-the-future".into()),
            duration_ms: None,
        };
        assert_eq!(q.interpolation(), Interpolation::Step);
    }

    #[test]
    fn linear_is_the_only_spelling_that_selects_linear() {
        for s in ["linear", "LINEAR", "Linear"] {
            let q = TimelineQuery {
                interpolation: Some(s.into()),
                duration_ms: None,
            };
            // Case-insensitive: a query string is not a place to be pedantic,
            // and a rejected value silently falls back to `step` -- which would
            // look like the feature not working rather than like a typo.
            assert_eq!(q.interpolation(), Interpolation::Linear, "{s}");
        }
    }

    /// Zero is "not asked for", not "a zero-length script". Clamping to zero
    /// would erase every action, and a player would show a static timeline for
    /// a script the user can see is fine.
    #[test]
    fn a_zero_or_absent_duration_means_the_whole_script() {
        assert_eq!(TimelineQuery::default().clamped_duration(), 0);
        assert_eq!(
            TimelineQuery {
                interpolation: None,
                duration_ms: Some(0)
            }
            .clamped_duration(),
            0
        );
        assert_eq!(
            TimelineQuery {
                interpolation: None,
                duration_ms: Some(1_500)
            }
            .clamped_duration(),
            1_500
        );
    }
}
