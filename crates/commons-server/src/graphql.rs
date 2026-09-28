//! `POST /graphql` — the four operations the UI sends, resolved by hand.
//!
//! T-P6-008. Spec `docs/spec/t-p6-008-graphql.md`, plan Step 3. The wire types
//! and the dispatcher are in `commons-api::graphql`; this file is the server
//! half, and it holds **no policy** — it resolves an identity, calls the store,
//! and maps a store error onto a message the client can show.
//!
//! # The consent gate is the whole point of this file
//!
//! Every resolver begins the same way: resolve the `CallerId` from the
//! **request**, the same way every REST route does, and pass it into the store.
//! A resolver that read the store without a caller would return the owner's
//! library to everyone, and nothing about the code would look wrong — there is
//! no `#[route]` attribute to review, which is exactly why it needs a test
//! rather than a reader.
//!
//! **There is no `local_caller()` fallback here, and no helper that takes an
//! `Option<HeaderMap>`.** `caller_from_request` itself falls back internally
//! when there is no bearer token, and that is the one fallback that is
//! correct: an unauthenticated request is the owner's own local request, which
//! is what `media::local_caller` exists to say. A *resolver* inventing an
//! identity is a different act from the identity layer declining to.
//!
//! # `after:` is decoded, and a bad cursor is still an explicit error
//!
//! This module used to refuse **every** non-null `after`, and the refusal was
//! right when it was written: keyset paging had never crossed a process
//! boundary, `Cursor`'s only constructor was `#[cfg(test)] pub(crate)`, and
//! nothing in the workspace serialized a `Cursor`. The filter had
//! `to_url`/`from_url` and the cursor had neither, because no surface had ever
//! needed one.
//!
//! **T-P6-009 gave the cursor a wire form** (`Cursor::to_url` / `from_url` in
//! `commons-store`), so `after` is now decoded against the sort it was made
//! for and honoured, and `endCursor` carries a real cursor.
//!
//! The principle underneath is unchanged, and it applies one level down:
//!
//! - **A cursor that cannot be decoded is an error, never a first page.**
//!   Silently ignoring `after` is the worst outcome available — a client paging
//!   with it receives page one forever, which looks like a filter that matches
//!   everything and a scroll that does nothing. That is why the decode failure
//!   returns an error naming the cause rather than an empty `nodes` array.
//! - **The refusal message CHANGED rather than disappearing, and the reason
//!   matters.** "Not available yet" told a client the server lacked a feature.
//!   "This cursor was made for a different sort" tells it *the client* changed
//!   its sort mid-scroll — a different cause, and a different remedy: restart
//!   the walk rather than retry. A client cannot tell those apart if the text
//!   is reused, and reusing it would have been the easy thing to do.
//!
//! **A cursor is a position, not a capability.** It is bound to its sort by a
//! fingerprint (T-P6-009 §2) and carries no caller, so it cannot widen
//! visibility: the consent clause is re-evaluated per query from the caller's
//! own grants. `resuming_does_not_widen_visibility` in
//! `tests/after_paging.rs` is the test that holds that honest — a cursor that
//! cached the first page's filter would pass every other test in the suite.
//!
//! The **first page** — what the UI shows on load — needs none of this, and
//! still works exactly as before.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use commons_api::graphql::{
    self, BulkApplyTagResult, BulkTarget, BulkTargetKind, CreateAllMissingResult, GqlErr,
    GqlRequest, GqlResponse, ObjectConnection, ObjectNode, PageInfo, PageInput, TagRef,
};
use commons_store::filter_ast::{CallerId, Filter};
use commons_store::sort::{Sort, SortKey, SortOrder};

use crate::{identity, AppState};

/// The page size ceiling, matching the store's own `MAX_PAGE`.
///
/// Enforced here as well as in the store because this value comes off the wire:
/// a client asking for a million rows should be refused by the surface that
/// received the request, with a message about the request, rather than by a
/// deeper layer whose error reads like a store failure.
const MAX_FIRST: usize = 200;

/// The default page size when the client sends none.
///
/// Bounded rather than "everything" for the same reason `query.rs` refuses a
/// `COUNT(*)`: this query is on the hot path and an unbounded page is a
/// decision the client did not make deliberately.
const DEFAULT_FIRST: usize = 50;

// ---------------------------------------------------------------------------
// The route
// ---------------------------------------------------------------------------

/// `POST /graphql`.
///
/// Deliberately **not** under `/api/v1`: it is the UI's transport, not a
/// versioned public script surface, and the OpenAPI document describes
/// `/api/v1`. Putting it inside `v1_routes` would add a POST endpoint to the
/// document that carries the compatibility promise.
/// `the_graphql_endpoint_is_not_under_api_v1` asserts it.
pub async fn graphql_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // The body is read as a String rather than via `Json`, because a GraphQL
    // request that fails to parse is a *GraphQL* error (a 200 with an `errors`
    // array), not an HTTP 400: the client's `query()` reads `errors` and shows
    // the message, and a 4xx would lose it behind a transport failure.
    let request: GqlRequest = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            return graphql_reply(GqlResponse::error(format!(
                "could not read the GraphQL request: {e}"
            )))
        }
    };

    // The identity is resolved ONCE, here, and passed down. Resolving it inside
    // each resolver instead would be four chances to forget it, and the one
    // that forgets is the bug.
    let caller = identity::caller_from_request(&state, &headers).await;

    let response = dispatch(&state, &caller, &request).await;
    graphql_reply(response)
}

fn graphql_reply(response: GqlResponse) -> Response {
    // HTTP 200 for a resolver error, deliberately. See the module docs.
    (StatusCode::OK, Json(response)).into_response()
}

async fn dispatch(state: &AppState, caller: &CallerId, request: &GqlRequest) -> GqlResponse {
    let Some(name) = graphql::operation_name(&request.query) else {
        return GqlResponse::error(
            "this document names no operation. Send `query Name(...)` or \
             `mutation Name(...)` -- an anonymous document cannot be dispatched.",
        );
    };

    // The name is matched against the client's four, case-sensitively, because
    // GraphQL operation names are case-sensitive by spec and a case-insensitive
    // match would make `objects` and `Objects` the same operation.
    match name {
        "Objects" => objects(state, caller, request).await,
        "BulkTags" => bulk_tags(state).await,
        "BulkApplyTag" => bulk_apply_tag(state, caller, request).await,
        "CreateAllMissing" => create_all_missing(state, caller, request).await,
        other => GqlResponse::error(format!(
            "unknown operation `{other}`. This endpoint serves exactly four: \
             Objects, BulkTags, BulkApplyTag, CreateAllMissing."
        )),
    }
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

async fn objects(state: &AppState, caller: &CallerId, request: &GqlRequest) -> GqlResponse {
    let input: PageInput = match variable(request, "input") {
        Ok(v) => v,
        Err(e) => return GqlResponse::error(e.message()),
    };

    let limit = match effective_limit(input.first) {
        Ok(n) => n,
        Err(e) => return GqlResponse::error(e.message()),
    };

    let filter = match decode_filter(input.filter.as_deref()) {
        Ok(f) => f,
        Err(e) => return GqlResponse::error(e.message()),
    };
    let sort = match decode_sort(input.sort.as_deref(), input.direction.as_deref()) {
        Ok(s) => s,
        Err(e) => return GqlResponse::error(e.message()),
    };

    // `after` is decoded HERE, after the sort, and that order is the design
    // rather than an accident of where the code sat. A cursor carries no sort,
    // so `from_url` has to be *told* which sort it is being decoded for, and
    // the only place that sort exists is the line above. Decoding first would
    // mean decoding twice, or guessing and re-checking.
    //
    // The error names the CAUSE, which is what a client acts on. A refusal that
    // said only "bad cursor" leaves the client unable to tell "my cursor is
    // stale, restart" from "the server is broken, retry" -- and retrying a
    // sort-mismatched cursor forever is exactly the loop the old blanket
    // refusal existed to prevent. See the module docs.
    let after = match input.after.as_deref() {
        None => None,
        Some(raw) => match commons_store::sort::Cursor::from_url(raw, &sort) {
            Ok(c) => Some(c),
            Err(e) => {
                return GqlResponse::error(format!(
                    "bad `after` cursor: {e}. It is bound to the sort it was made \
                     for, so a cursor from another sort -- or from another \
                     server's cursor format -- must be discarded and the walk \
                     restarted from the first page."
                ))
            }
        },
    };

    // `tiers` is accepted and deliberately NOT applied. It is a narrowing hint
    // in the client's type, and every narrowing this server can honour is
    // already implied by the caller's own grants -- applying a client-supplied
    // tier list on top would be a *widening* dressed as a filter, because a
    // caller who passes `["public"]` would expect it to be able to see
    // everything public, and the consent clause is the only thing that decides
    // that. Refusing is honest; quietly honouring a hint that cannot be honoured
    // is not.
    if input.tiers.is_some() {
        tracing::debug!("graphql: `tiers` accepted and ignored; the consent clause decides");
    }

    // `AppState::store` is a FIELD, not a getter. Reading it as a method was
    // the first compile error here, and it is worth recording because the
    // struct's own definition (`pub store: Store`) invites the assumption.
    // `sort` is cloned, not borrowed: `query_sorted` takes it by value (it
    // owns the key list it will hand to the SQL builder), and the same sort is
    // needed afterwards to fingerprint the cursor this page returns. Cloning a
    // `Vec<(SortKey, SortOrder)>` of two `Copy` pairs is free at this size, and
    // the alternative -- fingerprinting from a re-parsed sort -- is a second
    // source of truth for what the sort was.
    let page = match state
        .store
        .query_sorted(&filter, caller, sort.clone(), after, limit)
        .await
    {
        Ok(p) => p,
        Err(e) => return GqlResponse::error(format!("could not read objects: {e}")),
    };

    let connection = ObjectConnection {
        nodes: page
            .rows
            .iter()
            .map(|row| ObjectNode {
                id: row.id.clone(),
                kind: row.kind.clone(),
                title: row.title.clone(),
                // The rest are `None` on purpose: `query_sorted` selects five
                // columns and the client selects fourteen. Spec §1b has the
                // per-field table, §5b the named follow-ups.
                ..ObjectNode::default()
            })
            .collect(),
        page_info: PageInfo {
            has_next_page: page.has_more,
            // Both still false/`None`, and the reason is unchanged: there is no
            // previous page without a cursor to go back to, and reporting
            // `hasPreviousPage: true` would have the UI render a back arrow
            // that cannot work. Forward paging does not change that — a cursor
            // forward is not a cursor backward.
            has_previous_page: false,
            start_cursor: None,
            // The real cursor, fingerprinted against the sort this page was
            // read with. `next_cursor()` is `None` whenever `has_more` is
            // false, including for a short final page, so the last page hands
            // back `null` for free and a client cannot seek past the end and
            // receive an empty page that looks like a filter matching nothing.
            end_cursor: page.next_cursor().map(|c| c.to_url(&sort)),
        },
        // Always `None`. Spec §3.
        total_count: None,
    };

    match serde_json::to_value(&connection) {
        Ok(v) => GqlResponse::data(serde_json::json!({ "objects": v })),
        Err(e) => GqlResponse::error(format!("could not encode the object page: {e}")),
    }
}

// ---------------------------------------------------------------------------
// BulkTags
// ---------------------------------------------------------------------------

/// No `caller` parameter, and that is the design rather than an oversight.
///
/// §14.1 gates *objects*. A tag is a name in the owner's own taxonomy, and
/// there is no consent tier on a tag row — `tag` has no `consent_record`
/// join because there is nothing about a tag that a share link could grant or
/// withhold. Passing a `CallerId` here would be decoration.
///
/// It is also the one place a future reader should be most suspicious: an
/// operation that takes no identity looks exactly like a resolver that forgot
/// to take one, and the difference is invisible unless it is written down.
/// `tags_are_not_consent_gated` is the test that says so out loud.
async fn bulk_tags(state: &AppState) -> GqlResponse {
    let tags = match state.store.all_tags().await {
        Ok(t) => t,
        Err(e) => return GqlResponse::error(format!("could not read tags: {e}")),
    };
    let refs: Vec<TagRef> = tags
        .into_iter()
        .map(|t| TagRef {
            id: t.id,
            name: t.name,
        })
        .collect();
    match serde_json::to_value(&refs) {
        Ok(v) => GqlResponse::data(serde_json::json!({ "tags": v })),
        Err(e) => GqlResponse::error(format!("could not encode the tag list: {e}")),
    }
}

// ---------------------------------------------------------------------------
// BulkApplyTag
// ---------------------------------------------------------------------------

async fn bulk_apply_tag(state: &AppState, caller: &CallerId, request: &GqlRequest) -> GqlResponse {
    let tag_id = match variable::<String>(request, "tagId") {
        Ok(v) => v,
        Err(e) => return GqlResponse::error(e.message()),
    };
    let source = match optional_variable::<String>(request, "source") {
        Ok(v) => v,
        Err(e) => return GqlResponse::error(e.message()),
    };
    let target: BulkTarget = match variable(request, "target") {
        Ok(v) => v,
        Err(e) => return GqlResponse::error(e.message()),
    };

    // `excluded` is in the client's type and is **not** applied. The store's
    // `Target` has two variants and no exclusion list, so honouring it would
    // mean either writing the filter myself or ignoring the field. Ignoring a
    // field that looks like a filter is how a bulk edit deletes something the
    // user protected, so it is refused instead. See the doc comment on
    // `BulkTarget` in commons-api and the follow-up in the spec.
    if target.excluded.is_some() {
        return GqlResponse::error(
            "`excluded` is not supported by this server's bulk write, and \
             ignoring it would apply the tag to objects you excluded. Send a \
             `target` with `kind: IDS` and an explicit `ids` list instead.",
        );
    }

    let filter = match decode_filter(target.query.as_deref()) {
        Ok(f) => f,
        Err(e) => return GqlResponse::error(e.message()),
    };

    // `ids` is hoisted OUT of the match arm because `Target::Ids` borrows it
    // for the lifetime of the returned `Target`, and a `let` inside the arm
    // would die at the arm's end. The borrow checker caught this, which is the
    // right outcome: the alternative -- building the Target inside the arm and
    // returning it from the function -- is the same code and does not compile
    // either.
    let ids = target.ids.clone().unwrap_or_default();

    let store_target: commons_store::bulk::Target<'_> = match target.kind {
        BulkTargetKind::Ids => {
            // `ids: []` is the hazard the client's own type documents: it is a
            // request to tag NOTHING, and a server that treated an empty list
            // as an absent filter would tag the whole library. `Target::Ids`
            // with an empty slice does the right thing, but the guard is here so
            // the intent is explicit rather than dependent on the store's
            // internals.
            if ids.is_empty() {
                return GqlResponse::error(
                    "`target.ids` is empty. That means tag nothing; it does not \
                     mean every object. Send an explicit list, or use \
                     `kind: QUERY` with a filter if you meant the whole library.",
                );
            }
            commons_store::bulk::Target::Ids(&ids)
        }
        BulkTargetKind::Query => {
            if target.query.is_none() {
                return GqlResponse::error(
                    "`target.kind` is QUERY but no `query` filter was supplied.",
                );
            }
            commons_store::bulk::Target::Filter(&filter)
        }
    };

    let outcome = match state
        .store
        .bulk_apply_tag(&store_target, &tag_id, None, source.as_deref(), caller)
        .await
    {
        Ok(o) => o,
        Err(e) => return GqlResponse::error(format!("bulk tag failed: {e}")),
    };

    // `skippedInvisible` is the reason the caller is passed through at all: a
    // caller who names 40 objects and sees `applied: 3, skippedInvisible: 37` is
    // being told the truth. An error would be a lie about permissions, and the
    // success count alone would be a lie about the request.
    let result = BulkApplyTagResult {
        applied: outcome.applied,
        skipped_invisible: outcome.skipped_invisible,
        requested: outcome.requested,
    };
    match serde_json::to_value(result) {
        Ok(v) => GqlResponse::data(serde_json::json!({ "bulkApplyTag": v })),
        Err(e) => GqlResponse::error(format!("could not encode the bulk result: {e}")),
    }
}

// ---------------------------------------------------------------------------
// CreateAllMissing
// ---------------------------------------------------------------------------

/// The `kind` a title-only row is created with.
///
/// This is a **decision with a consequence**, not a default, and it belongs in
/// the id: `derive_id` hashes `kind` alongside the title, so the same title
/// under a different `kind` is a different object. The client sends titles
/// only — its own doc says so: "`rows` are titles, not ids, and deliberately so
/// — the id is derived from the row's content on the server, so a client that
/// computed it would have a second implementation of the derivation to keep in
/// step."
///
/// So the kind has to be chosen here, and the choice is `scene`, matching the
/// import path's own tests. A better answer is a real kind on the wire; this
/// constant is the interim one and it is named so a reader can find it.
const TITLE_ONLY_KIND: &str = "scene";

async fn create_all_missing(
    state: &AppState,
    caller: &CallerId,
    request: &GqlRequest,
) -> GqlResponse {
    let rows: Vec<String> = match variable(request, "rows") {
        Ok(v) => v,
        Err(e) => return GqlResponse::error(e.message()),
    };

    let drafts: Vec<commons_store::create::ObjectDraft> = rows
        .iter()
        .map(|title| commons_store::create::ObjectDraft::new(TITLE_ONLY_KIND).title(title.clone()))
        .collect();

    let outcome =
        match commons_store::create::create_all_missing(&state.store, &drafts, caller).await {
            Ok(o) => o,
            Err(e) => return GqlResponse::error(format!("could not create rows: {e}")),
        };

    // **The caller is passed and the store ignores it** — `create_all_missing`
    // ends with `let _ = caller;`, and `CreateOutcome::refused` is documented as
    // "Always 0 for a local single-user build". That is honest for a local
    // library and wrong for a shared one, and this is the first GraphQL *write*
    // path, so it is the place where that would first matter. The `caller` is
    // threaded through anyway, so the day the store enforces it nothing here
    // changes; and the refusal count is reported as the store computes it rather
    // than hard-coded to 0, so a store that starts refusing is visible instead
    // of being flattened.
    let result = CreateAllMissingResult {
        created: outcome.created,
        existing: outcome.existing,
        refused: outcome.refused,
    };
    match serde_json::to_value(result) {
        Ok(v) => GqlResponse::data(serde_json::json!({ "createAllMissing": v })),
        Err(e) => GqlResponse::error(format!("could not encode the create result: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Decoding: where a client string becomes a trusted value
// ---------------------------------------------------------------------------

/// A required variable from the request.
fn variable<T: serde::de::DeserializeOwned>(request: &GqlRequest, name: &str) -> Result<T, GqlErr> {
    let value = &request.variables;
    if value.is_null() {
        return Err(GqlErr::new(format!(
            "`{name}` is required and was not supplied."
        )));
    }
    let field = value.get(name).ok_or_else(|| {
        GqlErr::new(format!(
            "`{name}` is required by this operation. The client sent: {value}"
        ))
    })?;
    serde_json::from_value(field.clone())
        .map_err(|e| GqlErr::new(format!("`{name}` is the wrong shape: {e}")))
}

/// An optional variable, `None` when absent or null.
fn optional_variable<T: serde::de::DeserializeOwned>(
    request: &GqlRequest,
    name: &str,
) -> Result<Option<T>, GqlErr> {
    match request.variables.get(name) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| GqlErr::new(format!("`{name}` is the wrong shape: {e}"))),
    }
}

/// The page size, defaulted and capped.
///
/// Both bounds are here rather than left to the store because the number comes
/// off the wire, and a cap the client cannot see is a cap the client cannot
/// work around politely.
fn effective_limit(first: usize) -> Result<usize, GqlErr> {
    let n = if first == 0 { DEFAULT_FIRST } else { first };
    if n > MAX_FIRST {
        return Err(GqlErr::new(format!(
            "`first` is {n}; this server serves at most {MAX_FIRST} rows per page."
        )));
    }
    Ok(n)
}

/// Decode a §5.16 filter. **Errors rather than defaulting.**
///
/// The default would be "everything", and a malformed filter silently becoming
/// "everything" is a privacy bug wearing a parse error's clothes. The store's
/// own `Filter::from_url` is built for untrusted input and says so.
fn decode_filter(encoded: Option<&str>) -> Result<Filter, GqlErr> {
    let Some(encoded) = encoded else {
        return Ok(Filter::All);
    };
    Filter::from_url(encoded).map_err(|e| {
        GqlErr::new(format!(
            "`filter` is not a valid §5.16 filter: {e}. It was refused rather \
             than ignored, because ignoring it would show the whole library."
        ))
    })
}

/// Decode a sort key and direction. **Errors rather than defaulting.**
///
/// `sort.rs` is explicit that the allow-list is the part that gets forgotten:
/// "one interpolation of a caller-supplied sort name into an `ORDER BY` is an
/// injection, and §5.16's shareable URLs make the sort name
/// attacker-controlled by construction." So the key is matched against
/// `SortKey`'s own variants and anything else is refused — never interpolated.
fn decode_sort(key: Option<&str>, direction: Option<&str>) -> Result<Sort, GqlErr> {
    let order = match direction.unwrap_or("desc").to_ascii_lowercase().as_str() {
        "asc" => SortOrder::Asc,
        "desc" => SortOrder::Desc,
        other => {
            return Err(GqlErr::new(format!(
                "`direction` is `{other}`; this server accepts `asc` or `desc`."
            )))
        }
    };
    let key = match key.unwrap_or("date") {
        "date" => SortKey::Date,
        "title" => SortKey::Title,
        "kind" => SortKey::Kind,
        "addedAt" | "added_at" => SortKey::AddedAt,
        "ratingSum" | "rating_sum" => SortKey::RatingSum,
        other => {
            return Err(GqlErr::new(format!(
                "`sort` is `{other}`. Accepted: date, title, kind, addedAt, \
                 ratingSum. The key is matched against a fixed list and never \
                 interpolated into SQL."
            )))
        }
    };
    // `Sort::new` appends `id` as the trailing tiebreak, so the order is total
    // and a cursor is never ambiguous. It PANICS on an empty key list, which is
    // why the key is resolved to a variant above rather than collected.
    Ok(Sort::new(vec![(key, order)]))
}
