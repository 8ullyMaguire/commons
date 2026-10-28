//! Every route `commons-server` registers, with a one-line purpose.
//!
//! # Why a table, and why a test that reads the router
//!
//! The route list lives in `router`/`v1_routes` in `src/lib.rs`, and nothing
//! else describes it. That is fine until someone asks "what does this instance
//! actually serve?" — the answer then has to be reconstructed by reading a
//! builder chain, and the reconstruction is only as good as the reader.
//!
//! So this table is the answer, and `every_registered_route_is_documented` /
//! `every_documented_route_is_registered` make it an assertion rather than a
//! second opinion:
//!
//! * a route that exists and is not in this table is a door nobody has
//!   described — the test names it;
//! * an entry here for a route that is no longer registered is a description of
//!   something that is not there — the test names that too.
//!
//! Both directions matter for the same reason. A tested-in-one-direction table
//! drifts in the other direction silently, and the direction it drifts in is
//! whichever one the test does not look at.
//!
//! The test parses `src/lib.rs` itself — the `.route("…", …)` calls, their
//! container function, and the method chain — so adding a route without a line
//! here fails the build's test run, and no list of expected paths is maintained
//! beside the router. A hand-kept list of expected paths would be a *third*
//! description of the same surface and could agree with this table while both
//! disagreed with the router.
//!
//! # What is deliberately not here
//!
//! No audience column. `commons-server` has no session, token or scope concept
//! — `identity::local_caller()` is the whole of its authorization story — so an
//! audience field would be invention, and a wrong one: it would imply a door
//! model this crate does not have. Item 1.1 (an audience system) lives
//! elsewhere. Purpose alone is the claim this crate can actually make.
//!
//! # Paths are the REGISTERED paths
//!
//! Entries carry the path exactly as axum is given it (`:object_id`, not
//! `{object_id}`), and the path is the FULL one: `v1_routes` is nested at
//! `/api/v1` by `router`, so its entries are written with that prefix. Writing
//! the relative path here and prefixing at compare time would make the table
//! and the mount point two facts that have to stay in step.

/// One registered route: method, full path, one-line purpose.
///
/// A path with two methods is two entries, because the two answers differ — a
/// `PUT` is not a `GET` that happens to share a URL.
#[derive(Debug, Clone, Copy)]
pub struct RouteDoc {
    /// HTTP method, uppercase: `GET`, `POST`, `PUT`, `DELETE`, `PATCH`.
    pub method: &'static str,
    /// The path as registered, including any nesting prefix.
    pub path: &'static str,
    /// One line: what the route answers. Not a contract, not a schema — the
    /// OpenAPI document is the schema, and it only covers `/api/v1`.
    pub purpose: &'static str,
}

/// Every route this crate registers, in source order.
///
/// Source order rather than alphabetical, so a reader can follow it beside the
/// router. The test compares sets, so order is not a claim — it is only a
/// convenience, and no assertion here depends on it.
pub const ROUTE_DOC: &[RouteDoc] = &[
    // ------------------------------------------------------------------
    // Unversioned surface — `router` in `src/lib.rs`
    // ------------------------------------------------------------------
    RouteDoc {
        method: "GET",
        path: "/healthz",
        purpose: "Readiness: mode, database engine, schema version, pending migrations and jobs.",
    },
    RouteDoc {
        method: "GET",
        path: "/livez",
        purpose: "Liveness: the process is up. Does not touch the database.",
    },
    RouteDoc {
        method: "GET",
        path: "/metrics",
        purpose: "Prometheus text metrics; 404 unless the server was started with --metrics.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id",
        purpose: "The media bytes for one object, honouring range requests.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/caps",
        purpose: "Whether the file needs a proxy transcode at all, and which rung the client should ask for.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/proxy.m3u8",
        purpose: "HLS playlist for the on-demand transcode of an object; `?h=rung` picks a rung.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/playback",
        purpose: "The caller's stored playback state for the object; an unplayed object is a 200 with a fresh state, not a 404.",
    },
    RouteDoc {
        method: "PUT",
        path: "/media/:object_id/playback",
        purpose: "Persist playback position and loop state; an out-of-range value is 422 naming the field.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/subtitles",
        purpose: "The subtitle tracks found beside the object, with their format.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/transcript",
        purpose: "Transcript summary for the object: word count, duration, and the flags a UI needs to warn.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/transcript/words",
        purpose: "One page of transcript words, `?offset=&limit=`, with `truncated` to say there is more.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/chapters",
        purpose: "The object's chapters; an object with none is an empty list and a 200.",
    },
    RouteDoc {
        method: "GET",
        path: "/dlna/description.xml",
        purpose: "UPnP device description for DLNA clients that found the responder over SSDP. Names no object, so it is ungated.",
    },
    RouteDoc {
        method: "POST",
        path: "/dlna/control",
        purpose: "SOAP ContentDirectory Browse for DLNA clients; gated inside the handler, where the object id is.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/quotes",
        purpose: "One page of quotes for the object, using the same pager as the transcript words.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/topics",
        purpose: "Topic suggestions for the object, each marked as proposed; no topics is an empty list and a 200.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/speaker-clusters",
        purpose: "Speaker cluster proposals for the object's transcript.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/rejections",
        purpose: "Tag rejections recorded against the object, newest first.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/subtitles/:document_id.vtt",
        purpose: "One subtitle track as WebVTT; the document id is checked against this object, not trusted.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/funscripts",
        purpose: "The funscript files beside the object; absent and denied are both 404 so the list cannot be probed.",
    },
    RouteDoc {
        method: "GET",
        path: "/media/:object_id/funscripts/:funscript_id",
        purpose: "The parsed action timeline of one funscript, `?interpolation=` and `?duration_ms=` shaping the view only.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/share",
        purpose: "The share grants this instance has issued.",
    },
    RouteDoc {
        method: "POST",
        path: "/api/share",
        purpose: "Mint a share link — a capability grant — for an object.",
    },
    RouteDoc {
        method: "DELETE",
        path: "/api/share/:id",
        purpose: "Revoke a share grant; a second revoke is still a 200 so a retry is not a failure.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/s/:token",
        purpose: "Resolve a share token to its target, whether the bytes may be downloaded, and whether a password is still needed.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/s/:token/access",
        purpose: "The object row behind a share token, as far as that token's scope allows.",
    },
    RouteDoc {
        method: "POST",
        path: "/graphql",
        purpose: "The UI's GraphQL transport. Deliberately outside /api/v1: it is not a versioned public surface.",
    },
    // ------------------------------------------------------------------
    // Versioned surface — `v1_routes`, nested at `/api/v1`
    //
    // These are the same handlers served a second time under the prefix the
    // OpenAPI document describes. The share routes come out as
    // `/api/v1/api/...` because the handlers parse their own `/api` prefix and
    // re-rooting them would need a second set of handlers.
    // ------------------------------------------------------------------
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id",
        purpose: "The media bytes, re-served under the versioned surface; the copy the document describes.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/subtitles",
        purpose: "Subtitle tracks for the object, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/transcript",
        purpose: "Transcript summary for the object, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/transcript/words",
        purpose: "One page of transcript words, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/chapters",
        purpose: "Chapters for the object, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/quotes",
        purpose: "One page of quotes, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/topics",
        purpose: "Topic suggestions for the object, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/speaker-clusters",
        purpose: "Speaker cluster proposals, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/rejections",
        purpose: "Tag rejections for the object, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/subtitles/:document_id.vtt",
        purpose: "One subtitle track as WebVTT, under the versioned surface; the track suffix stays on the path.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/funscripts",
        purpose: "The funscript files beside the object, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/media/:object_id/funscripts/:funscript_id",
        purpose: "A funscript's parsed timeline, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/api/share",
        purpose: "The share grants this instance has issued, under the versioned surface.",
    },
    RouteDoc {
        method: "POST",
        path: "/api/v1/api/share",
        purpose: "Mint a share link, under the versioned surface.",
    },
    RouteDoc {
        method: "DELETE",
        path: "/api/v1/api/share/:id",
        purpose: "Revoke a share grant, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/api/s/:token",
        purpose: "Resolve a share token to its target, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/api/s/:token/access",
        purpose: "The capability-scoped object row behind a share token, under the versioned surface.",
    },
    RouteDoc {
        method: "GET",
        path: "/api/v1/openapi.json",
        purpose: "The OpenAPI 3.1 document for this surface. Served plain, and deliberately not described in itself.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// A route found in source: its enclosing function, its path, its methods.
    #[derive(Debug, PartialEq, Clone)]
    struct Registration {
        container: Option<String>,
        path: String,
        methods: Vec<String>,
    }

    const METHODS: [&str; 5] = ["get", "post", "put", "delete", "patch"];

    // --------------------------------------------------------------
    // A small Rust tokenizer
    //
    // Parsing the router with `find(".route(")` over the raw text is wrong in
    // a way that matters here: this file's own test module *contains* the
    // string `.route(` and the char literal `'"'`, so a text scan finds
    // registrations inside string literals and mis-reads a quote as the start
    // of a string. Tokenizing first means comments, string literals and char
    // literals are gone before anything looks for a call.
    // --------------------------------------------------------------

    #[derive(Debug, PartialEq, Eq, Clone, Copy)]
    enum Kind {
        Ident,
        Str,
        Punct(char),
    }

    #[derive(Debug, Clone)]
    struct Tok {
        kind: Kind,
        text: String,
    }

    /// `r"…"`, `r#"…"#` and friends. `i` points at the `r`.
    fn raw_string(c: &[char], i: usize) -> Option<(String, usize)> {
        let mut hashes = 0usize;
        let mut j = i + 1;
        while c.get(j) == Some(&'#') {
            hashes += 1;
            j += 1;
        }
        if c.get(j) != Some(&'"') {
            return None;
        }
        let start = j + 1;
        let mut k = start;
        while k < c.len() {
            if c[k] == '"' && (0..hashes).all(|h| c.get(k + 1 + h) == Some(&'#')) {
                return Some((c[start..k].iter().collect(), k + 1 + hashes));
            }
            k += 1;
        }
        None
    }

    fn tokens(src: &str) -> Vec<Tok> {
        let c: Vec<char> = src.chars().collect();
        let mut out = Vec::new();
        let mut i = 0usize;
        while i < c.len() {
            let ch = c[i];
            if ch.is_whitespace() {
                i += 1;
                continue;
            }
            if ch == '/' && c.get(i + 1) == Some(&'/') {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if ch == '/' && c.get(i + 1) == Some(&'*') {
                let mut depth = 0usize;
                while i < c.len() {
                    if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                        depth += 1;
                        i += 2;
                    } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            if ch == 'r' {
                if let Some((value, next)) = raw_string(&c, i) {
                    out.push(Tok {
                        kind: Kind::Str,
                        text: value,
                    });
                    i = next;
                    continue;
                }
            }
            if ch == '"' {
                let mut j = i + 1;
                let mut value = String::new();
                while j < c.len() && c[j] != '"' {
                    if c[j] == '\\' {
                        j += 1;
                        if let Some(cj) = c.get(j) {
                            value.push(*cj);
                        }
                        j += 1;
                    } else {
                        value.push(c[j]);
                        j += 1;
                    }
                }
                out.push(Tok {
                    kind: Kind::Str,
                    text: value,
                });
                i = j + 1;
                continue;
            }
            if ch == '\'' {
                // A char literal is skipped whole; a lifetime is just a tick.
                // `'"'` in the test module is exactly why this branch exists.
                if c.get(i + 1) == Some(&'\\') && c.get(i + 3) == Some(&'\'') {
                    i += 4;
                } else if c.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
                out.push(Tok {
                    kind: Kind::Punct('\''),
                    text: "'".to_string(),
                });
                continue;
            }
            if ch.is_alphanumeric() || ch == '_' {
                let mut j = i;
                let mut value = String::new();
                while j < c.len() && (c[j].is_alphanumeric() || c[j] == '_') {
                    value.push(c[j]);
                    j += 1;
                }
                out.push(Tok {
                    kind: Kind::Ident,
                    text: value,
                });
                i = j;
                continue;
            }
            out.push(Tok {
                kind: Kind::Punct(ch),
                text: ch.to_string(),
            });
            i += 1;
        }
        out
    }

    /// The enclosing function's name at every token index.
    ///
    /// Brace matching rather than an indentation heuristic, and by name rather
    /// than by position: the prefix a route gets depends on which function it
    /// is registered in, and a route found in a function with no known prefix
    /// must fail loudly rather than be assumed unversioned.
    fn containers(toks: &[Tok]) -> Vec<Option<String>> {
        let mut out = Vec::with_capacity(toks.len());
        let mut depth = 0usize;
        let mut stack: Vec<(usize, String)> = Vec::new();
        let mut pending: Option<String> = None;
        let mut want_name = false;
        for t in toks {
            match (&t.kind, t.text.as_str()) {
                (Kind::Ident, "fn") => want_name = true,
                (Kind::Ident, name) if want_name => {
                    pending = Some(name.to_string());
                    want_name = false;
                }
                (Kind::Ident, _) => want_name = false,
                (Kind::Punct('{'), _) => {
                    depth += 1;
                    if let Some(name) = pending.take() {
                        stack.push((depth, name));
                    }
                }
                (Kind::Punct('}'), _) => {
                    if stack.last().map(|(d, _)| *d) == Some(depth) {
                        stack.pop();
                    }
                    depth = depth.saturating_sub(1);
                }
                _ => {}
            }
            out.push(stack.last().map(|(_, n)| n.clone()));
        }
        out
    }

    /// Every `.route("path", …)` call in `src`, with its method chain.
    fn registrations(src: &str) -> Vec<Registration> {
        let toks = tokens(src);
        let ctx = containers(&toks);
        let mut out = Vec::new();
        for i in 0..toks.len() {
            if toks[i].kind != Kind::Ident || toks[i].text != "route" {
                continue;
            }
            if i == 0 || toks[i - 1].kind != Kind::Punct('.') {
                continue;
            }
            if toks.get(i + 1).map(|t| t.kind) != Some(Kind::Punct('('))
                || toks.get(i + 2).map(|t| t.kind) != Some(Kind::Str)
            {
                continue;
            }
            let path = toks[i + 2].text.clone();

            // The argument list, up to the `)` that closes `.route(` at
            // nesting depth zero. `get(a).put(b)` and `delete(h)` both come out
            // of this: a method is an ident immediately followed by `(`.
            let mut depth = 1usize; // the `(` after `route`
            let mut j = i + 3;
            let mut methods: Vec<String> = Vec::new();
            while j < toks.len() {
                match toks[j].kind {
                    Kind::Punct('(') => depth += 1,
                    Kind::Punct(')') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Kind::Ident
                        if METHODS.contains(&toks[j].text.as_str())
                            && toks.get(j + 1).map(|t| t.kind) == Some(Kind::Punct('(')) =>
                    {
                        let m = toks[j].text.to_uppercase();
                        if !methods.contains(&m) {
                            methods.push(m);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }

            out.push(Registration {
                container: ctx[i].clone(),
                path,
                methods,
            });
        }
        out
    }

    /// Where a route registered in `container` is actually mounted, or `None`
    /// if this test does not know that container.
    ///
    /// A route in an unknown function is not guessed at: the caller asserts.
    fn mount_prefix(container: Option<&str>) -> Option<&'static str> {
        match container {
            Some("router") => Some(""),
            // `router` does `.nest("/api/v1", v1_routes())`.
            Some("v1_routes") => Some("/api/v1"),
            _ => None,
        }
    }

    fn lib_rs() -> String {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lib.rs"))
            .expect("src/lib.rs must be readable for the route-documentation test")
    }

    /// `METHOD /full/path` for every route the router registers.
    fn registered() -> BTreeSet<String> {
        let found = registrations(&lib_rs());
        let mut out = BTreeSet::new();
        for reg in &found {
            let prefix = mount_prefix(reg.container.as_deref()).unwrap_or_else(|| {
                panic!(
                    "route {path} is registered in an unknown function, so this test cannot \
                     know its mount point. Add the container to `mount_prefix`.",
                    path = reg.path
                )
            });
            assert!(
                !reg.methods.is_empty(),
                "{prefix}{path} parsed with no method — the method-chain scan is broken",
                path = reg.path
            );
            for m in &reg.methods {
                out.insert(format!("{m} {prefix}{path}", path = reg.path));
            }
        }
        // A floor, not a count: if the walk stops finding routes the two
        // direction checks below compare empty sets and pass, which is the
        // failure mode a zero-route parse produces and the one that looks like
        // coverage. The number is a lower bound so ordinary additions do not
        // touch it.
        assert!(
            out.len() >= 40,
            "only {} route(s) parsed from src/lib.rs — the source scan is broken, and a \
             drift test that parses nothing compares nothing",
            out.len()
        );
        out
    }

    /// `METHOD /full/path` for every entry in the table.
    fn documented() -> BTreeSet<String> {
        ROUTE_DOC
            .iter()
            .map(|d| format!("{} {}", d.method, d.path))
            .collect()
    }

    // --------------------------------------------------------------
    // The parser's own test
    // --------------------------------------------------------------

    /// Every registration shape the parser claims to support, asserted.
    ///
    /// Without this the two direction checks have no floor under them either:
    /// a parser that quietly stopped understanding `axum::routing::delete(…)`
    /// would report that route as methodless, or a parser that mis-read a
    /// multi-line `.route(` would see one route where the router has two, and
    /// the documentation test would "pass" for the wrong reason.
    #[test]
    fn the_parser_handles_every_registration_shape_it_claims_to_support() {
        let shapes = [
            // One route, one method, same line.
            (r#"fn router() { Router::new().route("/a", get(h)) }"#, vec![("router", "/a", vec!["GET"])]),
            // Multi-line, trailing comma — the form rustfmt produces.
            (
                "fn router() {\n  Router::new().route(\n    \"/b\",\n    get(h),\n  )\n}",
                vec![("router", "/b", vec!["GET"])],
            ),
            // Two methods chained, GET then PUT.
            (
                r#"fn router() { Router::new().route("/c", get(a).put(b)) }"#,
                vec![("router", "/c", vec!["GET", "PUT"])],
            ),
            // Two methods chained in the other order — the parser must keep
            // source order rather than canonicalise it.
            (
                r#"fn router() { Router::new().route("/d", post(a).get(b)) }"#,
                vec![("router", "/d", vec!["POST", "GET"])],
            ),
            // Fully qualified method helper.
            (
                r#"fn router() { Router::new().route("/e", axum::routing::delete(h)) }"#,
                vec![("router", "/e", vec!["DELETE"])],
            ),
            // Namespaced handler and a param with a literal suffix.
            (
                r#"fn router() { Router::new().route("/f/:id.vtt", get(crate::m::h).patch(k)) }"#,
                vec![("router", "/f/:id.vtt", vec!["GET", "PATCH"])],
            ),
            // The nested container is reported by name, so the prefix can be
            // applied by the caller rather than guessed.
            (
                r#"fn v1_routes() -> Router<X> { Router::new().route("/g", get(h)) }"#,
                vec![("v1_routes", "/g", vec!["GET"])],
            ),
            // A `.route(` inside a line comment is not a registration.
            (
                "fn router() {\n  // .route(\"/ghost\", get(x))\n  Router::new().route(\"/real\", get(h))\n}",
                vec![("router", "/real", vec!["GET"])],
            ),
            // Nor is one inside a block comment.
            (
                "fn router() {\n  /* .route(\"/ghost\", get(x)) */\n  Router::new().route(\"/real\", get(h))\n}",
                vec![("router", "/real", vec!["GET"])],
            ),
            // Nor one inside a string literal — this file's own test module
            // searches for the string ".route(", so this shape is real.
            (
                r#"fn router() { let s = ".route(\"/ghost\", get(x))"; Router::new().route("/real", get(h)) }"#,
                vec![("router", "/real", vec!["GET"])],
            ),
            // Nor one inside a raw string.
            (
                "fn router() { let s = r#\".route(\\\"/ghost\\\", get(x))\"#; Router::new().route(\"/real\", get(h)) }",
                vec![("router", "/real", vec!["GET"])],
            ),
            // A char literal holding a quote does not open a string — the
            // existing `.find('\"')` in this crate's test module is why.
            (
                r#"fn router() { let q = '"'; Router::new().route("/h", get(h)) }"#,
                vec![("router", "/h", vec!["GET"])],
            ),
            // A closure's braces do not close the enclosing function, so a
            // route after one still reports its container.
            (
                r#"fn v1_routes() { let f = || { 1 }; Router::new().route("/i", get(h)) }"#,
                vec![("v1_routes", "/i", vec!["GET"])],
            ),
        ];

        for (src, expected) in shapes {
            let got: Vec<(String, String, Vec<String>)> = registrations(src)
                .into_iter()
                .map(|r| (r.container.unwrap_or_default(), r.path, r.methods))
                .collect();
            let expected: Vec<(String, String, Vec<String>)> = expected
                .into_iter()
                .map(|(c, p, m)| {
                    (
                        c.to_string(),
                        p.to_string(),
                        m.into_iter().map(String::from).collect(),
                    )
                })
                .collect();
            assert_eq!(got, expected, "shape not parsed as expected:\n{src}");
        }
    }

    // --------------------------------------------------------------
    // The two directions
    // --------------------------------------------------------------

    #[test]
    fn every_registered_route_is_documented() {
        let declared = registered();
        let described = documented();
        let missing: Vec<&String> = declared.difference(&described).collect();
        assert!(
            missing.is_empty(),
            "{} route(s) are registered and have no entry in `ROUTE_DOC`:\n  {}\n\
             A route nobody has described is a route nobody knows to keep working.",
            missing.len(),
            missing
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    #[test]
    fn every_documented_route_is_registered() {
        let declared = registered();
        let described = documented();
        let extra: Vec<&String> = described.difference(&declared).collect();
        assert!(
            extra.is_empty(),
            "{} entry/entries in `ROUTE_DOC` are not registered by the router:\n  {}\n\
             A description of a route that does not exist reads as a promise the \
             instance does not keep.",
            extra.len(),
            extra
                .iter()
                .map(|r| r.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }

    /// Two entries for one method+path would be one route with two purposes, and
    /// a set comparison cannot see it — the second copy would be invisible to
    /// both direction checks.
    #[test]
    fn the_table_has_no_duplicate_method_and_path_entries() {
        let mut seen = BTreeSet::new();
        for d in ROUTE_DOC {
            assert!(
                seen.insert(format!("{} {}", d.method, d.path)),
                "{} {} is documented twice",
                d.method,
                d.path
            );
        }
        assert_eq!(seen.len(), ROUTE_DOC.len());
    }

    /// The table is the readable answer to "what does this serve?", so every
    /// entry has to say something and name a method the router could register.
    #[test]
    fn every_entry_has_a_purpose_and_a_real_method() {
        const CANONICAL: [&str; 5] = ["GET", "POST", "PUT", "DELETE", "PATCH"];
        for d in ROUTE_DOC {
            assert!(
                d.purpose.len() > 20,
                "{} {} has no useful purpose: {:?}",
                d.method,
                d.path,
                d.purpose
            );
            assert!(
                !d.purpose.trim().is_empty() && !d.purpose.contains("TODO"),
                "{} {} is not described yet: {:?}",
                d.method,
                d.path,
                d.purpose
            );
            assert!(
                CANONICAL.contains(&d.method),
                "{} {} names a method axum's method router does not have",
                d.method,
                d.path
            );
            assert!(
                d.path.starts_with('/'),
                "{path} is not an absolute path; a registered path always starts at the root",
                path = d.path
            );
        }
    }
}
