//! The GraphQL wire types, and the dispatch that resolves them.
//!
//! T-P6-008, spec `docs/spec/t-p6-008-graphql.md`. **There is no GraphQL
//! library here, and that is a measured decision, not an omission** — see the
//! spec §5. `async-graphql` 7 is unusable against this workspace from both
//! ends: every version from 7.0.17 up declares axum 0.8, so its
//! `GraphQLRequest` cannot satisfy axum 0.7's `Handler` bound, and 7.0.0 — the
//! only one that declares axum 0.7 — does not compile on this toolchain at all
//! (149 `E0195` errors inside its own `model/directive.rs`).
//!
//! # What this costs, stated rather than glossed
//!
//! No introspection, so no generated client and no GraphiQL. No type-system
//! validation, so a `BulkTarget` with neither `ids` nor `query` is a runtime
//! check rather than a schema rejection. And field aliases do not work. The UI
//! sends none of these, which is what makes four operations viable at this
//! size — and a fifth operation, or any consumer wanting introspection, is a
//! reason to revisit the decision rather than extend this file.
//!
//! # Every field name here is load-bearing
//!
//! The client speaks camelCase (`coverPath`, `hasNextPage`, `pageInfo`,
//! `totalCount`) and **serde does not derive that from a snake_case field**. A
//! wrong name is not an error here: it is a `null` the UI renders as "no cover"
//! forever, or a field the client asked for that never arrives. So
//! `rename_all = "camelCase"` appears on every output struct, and
//! `a_bulk_target_round_trips_the_client_exact_shape` in
//! `crates/commons-server/tests/graphql_route.rs` is the test that makes the
//! mapping true rather than probable. A library would have generated these
//! mappings; a hand-rolled resolver means a person did, and the test has to
//! prove they got it right.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// A bulk-edit target.
///
/// `kind` is required and named rather than inferred from which field is set,
/// because `ids: []` and "no ids given" are different things — and `ids: []` is
/// the dangerous one. It is a request to tag nothing, and a server that treated
/// an empty list as an absent filter would tag **the entire library**. The
/// client already models it this way, and
/// `an_empty_id_list_tags_nothing_rather_than_everything` is why.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BulkTarget {
    /// The wire value is `IDS` or `QUERY`, spelled exactly as the client spells
    /// it. `rename_all = "SCREAMING_SNAKE_CASE"` is the one guessed convention
    /// allowed in this file, and the round-trip test is what makes it true.
    pub kind: BulkTargetKind,
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub excluded: Option<Vec<String>>,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BulkTargetKind {
    Ids,
    Query,
}

/// A page request.
///
/// **There is deliberately no `offset` field.** The client's `PageInput` has
/// none either, and the pair of those is a type-level guarantee that "skip to
/// page 400" is inexpressible. Adding one "for symmetry" would restore exactly
/// the problem `commons-store/src/query.rs` refuses to solve, and the store's
/// own docs say why: a keyset page cannot produce an honest exact position.
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageInput {
    pub first: usize,
    /// Opaque. Never parsed by the client, never constructed by hand.
    #[serde(default)]
    pub after: Option<String>,
    /// §5.16: the serialized filter, as a JSON string.
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub direction: Option<String>,
    /// §14.1. Absent means "whatever the caller may see" — the consent clause
    /// decides, and the caller's own grants bound it. This is a *narrowing*
    /// hint, never a widening one.
    #[serde(default)]
    pub tiers: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// One row of the object grid.
///
/// **Most of these fields are `None`, and that is the honest answer rather
/// than a stub.** The client selects 14 fields; `query_sorted` selects 5
/// columns. `coverPath`, `width`, `height`, `folder` and `performers` have no
/// column anywhere in the schema, `rating` has a sum and a count rather than a
/// rating, and `durationMs` would need a per-row sum over `segment`. Spec §1b
/// has the per-field table and §5b has the named follow-ups (F1–F7) for each.
///
/// The alternative — inventing a `rating` rule to fill the shape — would be
/// worse than a visible `null`, because an invented aggregate that disagrees
/// with the UI's is a data bug wearing a transport's clothes. The client's own
/// types permit all of it (`string | null`, `readonly string[]`), so a `null`
/// renders as absent data and the grid still works.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ObjectNode {
    pub id: String,
    pub kind: String,
    pub title: Option<String>,
    pub date: Option<String>,
    pub organized: Option<String>,
    pub cover_path: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_ms: Option<i64>,
    pub producer: Option<String>,
    pub performers: Vec<String>,
    pub tags: Vec<String>,
    pub folder: Option<String>,
    /// See spec §3. There is no column and no honest aggregate; the client's
    /// `Connection<T>` types this `number | null` for exactly this case.
    pub rating: Option<f64>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageInfo {
    pub has_next_page: bool,
    pub has_previous_page: bool,
    pub start_cursor: Option<String>,
    pub end_cursor: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct ObjectConnection {
    pub nodes: Vec<ObjectNode>,
    pub page_info: PageInfo,
    /// **Always `None`.** See spec §3 and the doc comment on the resolver.
    ///
    /// No `skip_serializing_if`, deliberately: the client asks for this field
    /// and expects it *present and null*. Omitting the key entirely is a
    /// different document, and that is the mistake a null-as-absent shortcut
    /// invites.
    pub total_count: Option<i64>,
}

#[derive(Serialize, Clone, Debug)]
pub struct TagRef {
    pub id: String,
    pub name: String,
}

#[derive(Serialize, Clone, Copy, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct BulkApplyTagResult {
    pub applied: usize,
    pub skipped_invisible: usize,
    pub requested: usize,
}

#[derive(Serialize, Clone, Copy, Debug, Default)]
pub struct CreateAllMissingResult {
    pub created: usize,
    pub existing: usize,
    pub refused: usize,
}

// ---------------------------------------------------------------------------
// The envelope
// ---------------------------------------------------------------------------

/// A GraphQL request, as the client sends it.
///
/// `query` and `variables` are the only fields the UI's `query()` helper
/// populates; `operationName` is accepted because the spec's shape includes it
/// and a client that sends it must not be rejected for it.
#[derive(Deserialize, Clone, Debug)]
pub struct GqlRequest {
    pub query: String,
    #[serde(default)]
    pub variables: serde_json::Value,
    #[serde(default, rename = "operationName")]
    pub operation_name: Option<String>,
}

/// A GraphQL response.
///
/// `data` is present-but-null on a resolver error, which is what the client
/// parses: `ui/src/lib/api/client.ts`'s `query()` reads `errors` and surfaces
/// the message. A 500 would lose the message behind a transport failure.
#[derive(Serialize, Clone, Debug)]
pub struct GqlResponse {
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<GqlError>>,
}

#[derive(Serialize, Clone, Debug)]
pub struct GqlError {
    pub message: String,
}

impl GqlResponse {
    /// A success carrying `data`.
    pub fn data(value: serde_json::Value) -> Self {
        Self {
            data: Some(value),
            errors: None,
        }
    }

    /// A resolver failure: HTTP 200, `data: null`, and a message the client can
    /// show. Never a status code, and never a bare 500.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            data: None,
            errors: Some(vec![GqlError {
                message: message.into(),
            }]),
        }
    }
}

/// An error a resolver returns.
///
/// Deliberately not a `Box<dyn Error>`: the message is the entire payload the
/// client sees, so it has to be written deliberately rather than inherited from
/// whatever `Display` happened to produce. Every constructor below names the
/// *client-supplied* thing that was wrong, because that is what a user can act
/// on — "unknown sort key `titel`" is actionable and "invalid input" is not.
#[derive(Debug, Clone)]
pub struct GqlErr(String);

impl GqlErr {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    pub fn message(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for GqlErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GqlErr {}

pub type GqlResult<T> = Result<T, GqlErr>;

// ---------------------------------------------------------------------------
// The operation name — the whole of the dispatch trick
// ---------------------------------------------------------------------------

/// Extract the operation name from a document.
///
/// Every document the UI sends names its operation (`query Objects(...)`,
/// `mutation BulkApplyTag(...)`), so dispatch is a scan for the first
/// `query`/`mutation` keyword followed by an identifier. **No GraphQL parser,
/// no document AST, and no way for a field to be silently dropped for want of
/// a selection-set implementation** — which is the deal this hand-rolled
/// resolver makes with the spec's "four known documents".
///
/// Fragments (`{ ...Name }`) and the shorthand `{ field }` carry no operation
/// name. The client sends neither, and both are reported as an error rather
/// than guessed at, because a wrong guess dispatches to the wrong resolver and
/// answers a different question than was asked.
pub fn operation_name(document: &str) -> Option<&str> {
    let bytes = document.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Skip strings and comments, so a `# query Fake` comment or a
        // `"query"` inside a string argument cannot name the operation.
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
            }
            b'q' | b'm' => {
                if let Some(name) = keyword_at(document, i) {
                    return Some(name);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

/// If `document[i..]` begins `query Name` or `mutation Name`, return `Name`.
///
/// The keyword and the name are separated by **optional whitespace**, and that
/// is not optional in practice: the UI's documents are
/// `query Objects($input: PageInput!)` and
/// `mutation BulkApplyTag($tagId: ID!)` — a space, always, because the
/// variable definitions follow. The first version of this required a name
/// character *immediately* after the keyword, so it matched `queryObjects` and
/// none of the four real documents. Three of the five tests caught it, and the
/// two that passed were the ones asserting a `None` — the classic shape where
/// a parser bug and a correct rejection look identical.
fn keyword_at(document: &str, i: usize) -> Option<&str> {
    let rest = document.get(i..)?;
    for keyword in ["query", "mutation"] {
        let Some(after) = rest.strip_prefix(keyword) else {
            continue;
        };
        // The character after the keyword decides, and it must be whitespace.
        // The real documents are `query Objects(...)` and
        // `mutation BulkApplyTag(...)` — a space, always, because variable
        // definitions follow.
        //
        // Requiring whitespace rather than merely "not a name character" is
        // what stops `{ querySaved }` dispatching to an operation called
        // `Saved`: with the looser check a field whose name begins `query` or
        // `mutation` becomes a dispatchable operation, and a test asserting
        // `None` caught exactly that. The looser version also matched
        // `queryObjects`, which is a field too.
        let next = after.as_bytes().first().copied()?;
        if !next.is_ascii_whitespace() {
            continue;
        }
        let trimmed = after.trim_start();
        let name_start = after.len() - trimmed.len();
        let name_len = trimmed
            .as_bytes()
            .iter()
            .take_while(|b| is_name_continue(**b))
            .count();
        if name_len == 0 {
            continue;
        }
        let start = i + keyword.len() + name_start;
        return document.get(start..start + name_len);
    }
    None
}

fn is_name_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_operations_the_client_sends_are_recognised() {
        // Verbatim shapes from ui/src/lib/api/client.ts.
        for (doc, want) in [
            ("query Objects($input: PageInput!) { objects(input: $input) { nodes { id } } }", "Objects"),
            ("query BulkTags { tags { id name } }", "BulkTags"),
            (
                "mutation BulkApplyTag($tagId: ID!, $target: BulkTarget!, $source: String) { bulkApplyTag(tagId: $tagId, target: $target, source: $source) { applied } }",
                "BulkApplyTag",
            ),
            (
                "mutation CreateAllMissing($rows: [String!]!) { createAllMissing(rows: $rows) { created } }",
                "CreateAllMissing",
            ),
        ] {
            assert_eq!(operation_name(doc), Some(want), "doc: {doc}");
        }
    }

    #[test]
    fn a_name_is_not_matched_inside_a_longer_word() {
        // `querySaved` is an object field, not an operation named `Saved`.
        assert_eq!(operation_name("{ querySaved }"), None);
        assert_eq!(operation_name("{ mutationLog }"), None);
    }

    #[test]
    fn a_commented_out_operation_does_not_name_the_document() {
        // The real document follows, so the comment must be skipped rather
        // than supplying the wrong name.
        let doc = "# query Decoy\nquery Real { objects(input: {}) { nodes { id } } }";
        assert_eq!(operation_name(doc), Some("Real"));
    }

    #[test]
    fn the_keyword_inside_a_string_argument_is_not_the_operation() {
        let doc = r#"{ search(term: "query Fake") { id } }"#;
        assert_eq!(operation_name(doc), None);
    }

    #[test]
    fn an_anonymous_document_has_no_operation_name_rather_than_a_guess() {
        // The shorthand form and a fragment spread both lack a name. Guessing
        // would dispatch to the wrong resolver.
        assert_eq!(
            operation_name("{ objects(input: {}) { nodes { id } } }"),
            None
        );
        assert_eq!(operation_name("fragment F on Object { id }"), None);
    }
}
