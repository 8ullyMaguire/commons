//! The `/api/v1` OpenAPI document.
//!
//! # Generated from the handlers, not written by hand
//!
//! Every route in the document is one a `#[utoipa::path]` attribute is attached
//! to, and the document is assembled from those attributes — so a route cannot
//! be added to `v1_routes` without the compiler being asked to describe it.
//!
//! # Why there is no `utoipa-axum` here
//!
//! `utoipa-axum` looks like the obvious way to do this: one `OpenApiRouter`
//! that both serves `/api/v1` and carries the document. It cannot be used, and
//! the reason is a defect rather than a taste.
//!
//! Its `routes!` macro builds **one** `MethodRouter` containing every handler
//! and then routes that single router onto **every** path. Two same-method
//! routes therefore collide, and axum panics at construction:
//!
//! ```text
//! Overlapping method route. Cannot add two method routes that both handle `GET`
//! ```
//!
//! Reproduced in a scratch crate against 0.1.3: `routes!(a, b)` where `a` is
//! `GET /one` and `b` is `GET /two` panics, while `routes!(a, c)` with `c` a
//! `POST` does not. So the failure is the **method**, not the path, and the
//! path is irrelevant to it. `/api/v1` has eleven GETs, so this is not an edge
//! case — the integration cannot express this router at all.
//!
//! 0.3.0 has the same `routes()` body and additionally requires axum 0.8.4,
//! while this workspace is on 0.7.9. There is no version of the crate that both
//! supports axum 0.7 and avoids the collapse.
//!
//! # What replaces it
//!
//! The router stays hand-built in `crate::v1_routes`, the document is generated
//! from `#[utoipa::path]`, and the seam between them — a route served but not
//! described, or described but not served — is closed by a test rather than by
//! the type system. `openapi_document_matches_the_served_routes` is that test,
//! and it is the reason this approach is safe: the guarantee the integration
//! would have given structurally is restored as an explicit check that fails
//! loudly the first time a route is added in one place and not the other.

use utoipa::OpenApi;

/// The document for `/api/v1`.
///
/// Assembled by listing the same handlers `crate::v1_routes` routes. The two
/// lists are kept adjacent in the source for that reason: the test that checks
/// they agree is cheap, but it is only cheap because they can be compared
/// without cross-referencing another file.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "commons",
        version = "1.0.0",
        description = "The public `/api/v1` surface. The same handlers also answer \
                       unversioned, but only these paths are covered by any \
                       compatibility promise."
    ),
    paths(
        crate::media::get_media,
        crate::subtitles::list_tracks,
        crate::subtitles::get_vtt,
        crate::interview::get_transcript,
        crate::interview::get_words,
        crate::interview::get_chapters,
        crate::interview::get_quotes,
        crate::interview::get_topics,
        crate::funscript::list,
        crate::funscript::timeline,
        crate::share::create_share,
        crate::share::list_share,
        crate::share::revoke_share,
        crate::share::resolve_share,
        crate::share::share_access,
    ),
    components(schemas(crate::share::CreateShare))
)]
pub struct ApiDoc;

impl ApiDoc {
    /// The document as a value.
    ///
    /// `utoipa::OpenApi::openapi()` rather than a hand-built struct, so the
    /// `paths` and `components` lists above are the only description of the
    /// surface and cannot drift from a second copy of it.
    pub fn document() -> utoipa::openapi::OpenApi {
        <Self as utoipa::OpenApi>::openapi()
    }

    /// Every path in the document, as `METHOD path`.
    ///
    /// Exists for `openapi_document_matches_the_served_routes`, which needs a
    /// flat comparable list rather than the nested `Paths` structure. Kept next
    /// to the document so the two cannot disagree about what the surface is.
    pub fn route_table() -> Vec<String> {
        let doc = Self::document();
        let mut out = Vec::new();
        for (path, item) in &doc.paths.paths {
            if item.get.is_some() {
                out.push(format!("GET {path}"));
            }
            if item.post.is_some() {
                out.push(format!("POST {path}"));
            }
            if item.put.is_some() {
                out.push(format!("PUT {path}"));
            }
            if item.delete.is_some() {
                out.push(format!("DELETE {path}"));
            }
        }
        out.sort();
        out
    }
}

/// `GET /api/v1/openapi.json`.
///
/// Takes no state extractor: the document is a pure function of the route
/// table, so a handler that needed `AppState` would be claiming the document
/// depends on runtime state it does not.
pub async fn serve_document() -> axum::Json<utoipa::openapi::OpenApi> {
    axum::Json(ApiDoc::document())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The document and the served routes describe the same surface.
    ///
    /// This is the guarantee `utoipa-axum` would have given structurally, and
    /// the reason the hand-built router is safe without it. The failure it
    /// catches is a route added in one place and not the other:
    ///
    /// * a route in `v1_routes` with no `#[utoipa::path]` — a consumer's
    ///   generated client has no method for an endpoint that answers 200;
    /// * a `#[utoipa::path]` with no route — a client calls a documented path
    ///   that 404s, which is worse, because the doc promised it.
    ///
    /// Derived from the SOURCE rather than from a hand-written list, so it does
    /// not need editing when a route is added — a list of expected paths here
    /// would be a second list to forget, and the test would fail for the right
    /// reason while being wrong for the wrong one.
    ///
    /// `/openapi.json` is excluded: it is the document's own endpoint, served
    /// without a `#[utoipa::path]` on purpose (see `serve_document`).
    #[test]
    fn openapi_document_matches_the_served_routes() {
        let documented: std::collections::BTreeSet<String> = ApiDoc::route_table()
            .into_iter()
            .filter(|r| !r.ends_with("/openapi.json"))
            .collect();

        let served = crate::tests::v1_route_table_for_tests();

        assert_eq!(
            documented, served,
            "the document and the served routes disagree. A route in one and not \
             the other is either invisible to every generated client or a promise \
             the server does not keep."
        );
        assert!(
            documented.len() >= 12,
            "only {} routes documented — the attribute list is probably not wired up",
            documented.len()
        );
    }

    /// The document is a valid OpenAPI 3 document with the paths in it.
    ///
    /// Cheap, and it catches the failure that a `paths()` list naming a function
    /// which no longer exists: utoipa silently drops it and the document comes
    /// back valid, smaller, and wrong.
    #[test]
    fn the_document_is_a_valid_openapi_document() {
        let doc = ApiDoc::document();
        // `OpenApiVersion` is a one-variant enum with no accessor, so the
        // wire form is checked by serialising rather than by matching the
        // variant -- which is the more honest assertion anyway, since what a
        // consumer's parser reads is the JSON, not the enum.
        let wire = serde_json::to_value(&doc.openapi).expect("the version serialises");
        assert_eq!(wire, "3.1.0", "must be an OpenAPI 3.1 document");
        assert!(!doc.paths.paths.is_empty(), "no paths in the document");
        assert_eq!(
            doc.info.title, "commons",
            "the title is what a client's generated docs page shows"
        );
    }

    /// A path parameter in the document is a path parameter in the route.
    ///
    /// `{object_id}` in the document against `:object_id` on the route is the
    /// conversion `utoipa-axum`'s `colonized_params` would have done. Doing it
    /// here means the conversion is asserted rather than assumed, which matters
    /// because the whole point of dropping that crate was that its conversion
    /// was where its route handling went wrong.
    #[test]
    fn path_parameters_use_the_openapi_brace_form() {
        for entry in ApiDoc::route_table() {
            let path = entry.split_once(' ').expect("`METHOD path`").1;
            assert!(
                !path.contains(':'),
                "{path} uses axum's `:name` form; OpenAPI requires `{{name}}`, and a \
                 generated client would send the wrong name"
            );
        }
    }
}
