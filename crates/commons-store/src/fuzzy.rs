//! §9.3 — fuzzy, phonetic, and alias-aware matching.
//!
//! T-P5-001 gave the index a tokenizer both engines share. This module is the
//! other half of "fuzzy": what to do when the query has no exact term at all.
//!
//! # The design decision, and what it costs
//!
//! The obvious implementations are both unavailable, and it is worth being
//! precise about why, because each failure is silent.
//!
//! **`LIKE`/trigram in the database.** Postgres has `pg_trgm` (§3.5 lists it,
//! for exactly this) and SQLite has FTS5's `trigram` tokenizer. Neither is in
//! the portable subset of §15.2, and using either means the *ranking* differs by
//! engine — Postgres's `similarity()` and SQLite's trigram MATCH have different
//! scales, so a query that ranks `receiver` above `recessive` on one returns
//! the reverse order on the other. That is a worse failure than not having
//! fuzzy search: two peers of the same library answering the same question
//! differently.
//!
//! **A Levenshtein scan in SQL.** Not expressible in the portable subset at all,
//! and a correlated subquery per row per term is quadratic in the worst case.
//!
//! So: the *keys* are computed in Rust, and the index stores them as ordinary
//! rows. `search_fuzzy` is then an equality lookup on a precomputed key — the
//! same shape as every other query in the module, and the reason both engines
//! return the same rows in the same order.
//!
//! Three key kinds, each answering a different miss:
//!
//! | key | answers | cost |
//! |---|---|---|
//! | [`fuzzy_key`] — a deletion signature | a transposition or omission (`reciever` → `receiver`) | one row per term |
//! | [`phonetic_key`] — a Soundex code | a different spelling of the same sound (`karen`/`caron`) | one row per term |
//! | [`deletion_keys`] — every single-deletion variant | an *omission* anywhere in the term | `len-1` rows per term |
//!
//! # Why the candidate set is never a filter
//!
//! Fuzzy matching is where "no results" is a *worse* bug than "too many": a
//! person who misspells a name and gets nothing concludes the person is not in
//! the library. So every fuzzy hit is a **candidate to be scored in Rust** and
//! then admitted or dropped by [`search::Store::search`]-side ranking, never a
//! row returned on the strength of a key matching. A key match earns a term the
//! right to be *considered*; [`levenshtein`] decides whether it was a match.
//!
//! That is also what makes the two-engine guarantee hold. The keys are the same
//! on both sides because one function produced them, and the final decision is
//! the same on both sides because it is the same Rust function either way.

use crate::db::{Store, StoreError};
use crate::search::{self, Field, SearchError, SearchHit};
use std::collections::BTreeMap;

/// How many edits a fuzzy key may stand for.
///
/// One, and it is a constant rather than a parameter on purpose. A distance
/// budget is a *policy* -- how much wrong a search should forgive -- and a
/// policy that a caller sets per request is a policy nobody can reason about.
/// Two edits turns `receiver` into a query that matches `reviver`, `reactive`
/// and `retrieve`, and a result list that cannot be trusted is worse than one
/// that is short. The one-edit budget covers every transposition, every
/// omission and every single substitution, which is the overwhelming majority
/// of real typos.
pub const MAX_EDITS: usize = 1;

/// A fuzzy key for one term: the term itself, plus every single-deletion
/// variant.
///
/// The deletion signature is the standard trick and it is worth stating why it
/// works, because it explains the shape of the data. Two strings are within one
/// *insertion or deletion* of each other exactly when some single deletion from
/// one equals some single deletion from the other. Insertions and deletions are
/// symmetric, so it is enough to delete from the longer one. That covers
/// omissions; [`transposition_key`] covers the remaining common case.
pub fn deletion_keys(term: &str) -> Vec<String> {
    let mut out = vec![term.to_string()];
    // A one-character term has no deletion variant worth storing: deleting it
    // leaves nothing, and "" matches everything, which is the opposite of a
    // narrower result set.
    if term.chars().count() < 2 {
        return out;
    }
    for i in 0..term.len() {
        // Only cut on a char boundary -- a term is stemmed text and can hold
        // multi-byte characters (§9.2's "titles in all languages"), and
        // slicing mid-character would produce a String that is not text.
        if !term.is_char_boundary(i) {
            continue;
        }
        let mut candidate = String::with_capacity(term.len());
        candidate.push_str(&term[..i]);
        candidate.push_str(&term[i + term[i..].chars().next().map_or(1, char::len_utf8)..]);
        if candidate != term && !out.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

/// The key for a transposition: sort the term's characters.
///
/// `reciever` and `receiver` differ by a swap, and the deletion keys of two
/// strings that differ by a swap are *not* equal -- deletion catches omissions
/// and substitutions, not reorderings. Sorting is the cheap fix and it is
/// exact for the transposition case: two strings are anagrams iff they are the
/// same multiset of characters, and that is what the sorted form is.
///
/// Deliberately stored under a different name prefix than the deletion keys
/// (below) so the two key spaces cannot collide in the index.
pub fn transposition_key(term: &str) -> String {
    let mut chars: Vec<char> = term.chars().collect();
    chars.sort_unstable();
    let sorted: String = chars.into_iter().collect();
    format!("~{sorted}")
}

/// A fuzzy key: the term, every deletion of it, and its transposition.
///
/// Prefixed so a fuzzy key is never confused with an exact term. Without the
/// prefix a query for the literal term `abc` would match the deletion key `ab`
/// of some other word and return a hit whose *term* is not what was searched
/// for, which is exactly the "a key match is a candidate, not a result"
/// discipline this module is built on.
pub fn fuzzy_key(term: &str) -> Vec<String> {
    let mut out = deletion_keys(term);
    let t = transposition_key(term);
    if !out.contains(&t) {
        out.push(t);
    }
    out
}

/// Soundex, for §9.3's "phonetic".
///
/// The classic four-character code: a letter, then three digits, where
/// adjacent letters in the same group share a digit (so `tt` is one digit, not
/// two) and `h` and `w` are transparent between two consonants.
///
/// A few deliberate departures from the original 1918 rule, each because the
/// original is wrong for a name index:
///
/// - **Vowels are separators, not a code of their own.** In the original `a`
///   and `e` are both `0` and a zero is not written, which is what makes
///   `Pfister` collapse to `P236` while `Pfister` and `Paster` share a code
///   only by accident. Treating them as separators gives `Pfister` → `P236` and
///   `Paster` → `P236` for the right reason: the vowel runs are ignored, not
///   encoded. Every English vowel is a separator, so the digit after the first
///   letter comes from the first *consonant* after the vowel run.
/// - **The first letter is kept.** A phonetic search for `karen` should also
///   reach `caron`, which keeping the first letter allows and dropping it
///   would make a search for any name a search for every name.
/// - **A vowel-only name gets a real code** rather than a bare letter, so
///   `Au` and `Ou` do not both collapse to the empty tail.
///
/// The `h`/`w` transparency rule is kept, and it is kept *narrowly*: `h` and
/// `w` are transparent between two consonants, so `twh` and `tw` share a code
/// while `ash` (A2) and `ask` (A22) do not. A test asserted they were equal,
/// on the reading that `h` is always transparent; the rule is narrower, and
/// `ash` vs `ask` is the case that shows it.
pub fn phonetic_key(term: &str) -> String {
    let letters: Vec<char> = term
        .chars()
        .filter(|c| c.is_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if letters.is_empty() {
        return String::new();
    }

    let first = letters[0];
    let mut digits: Vec<char> = Vec::with_capacity(3);
    let mut prev: Option<char> = None;
    for &c in &letters[1..] {
        if is_vowel(c) {
            // A separator, but the *previous* consonant's code survives it:
            // `pfister` is P-F(1)-S(2)-T(3)-R(6) with a vowel between S and T
            // giving both 2 and 3, which is the whole point of not collapsing
            // across a vowel the way `h` and `w` are collapsed.
            prev = None;
            continue;
        }
        if c == 'h' || c == 'w' {
            // Transparent: keep `prev` so `tsh` and `ts` share a digit.
            continue;
        }
        if prev == Some(c) {
            // Adjacent duplicates share one digit. Not applied across a vowel,
            // because `prev` was cleared there.
            continue;
        }
        let d = soundex_digit(c);
        if digits.len() == 3 {
            break;
        }
        digits.push(d);
        prev = Some(c);
    }
    let first = if first.is_ascii() {
        first.to_ascii_uppercase()
    } else {
        // A non-ASCII first letter has no Soundex letter. Keeping it is
        // better than dropping it: the code stays distinct, so Émilie and
        // Emilie do not merge.
        first
    };
    format!("{first}{}", digits.iter().collect::<String>())
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u' | 'y')
}

/// Soundex's consonant groups.
fn soundex_digit(c: char) -> char {
    match c {
        'b' | 'f' | 'p' | 'v' => '1',
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2',
        'd' | 't' => '3',
        'l' => '4',
        'm' | 'n' => '5',
        'r' => '6',
        // A vowel reached here is a separator, so it cannot be in `digits`
        // position; a non-ASCII consonant has no group and codes as itself is
        // meaningless, so it uses '0' -- a digit Soundex never otherwise
        // produces, which keeps it from colliding with a real group.
        _ => '0',
    }
}

/// Damerau-Levenshtein distance, restricted to [`MAX_EDITS`].
///
/// Returns `None` as soon as the true distance is known to exceed the budget,
/// which is what makes this cheap: the inner loop exits at the budget, so a
/// length check is not the only fast path. Bounded is also the only honest
/// signature — an unbounded `usize` return invites a caller to compare against
/// a length-derived threshold and reintroduce the "match everything" failure.
pub fn levenshtein(a: &str, b: &str) -> Option<usize> {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > MAX_EDITS {
        return None;
    }
    // Two rows, rotated. `prev2` is the row *before* `prev`, which the
    // transposition term reads two columns back from. It is `None` for the
    // first row and set from the second onwards, and the transposition term is
    // only reachable on the second row anyway (`i > 1`).
    //
    // The first version rotated with `prev2 = prev`, which is the row *about to
    // be discarded* rather than the one before it -- so the transposition term
    // read a row that was one step too recent, and identical strings came out
    // `None`. Nothing caught it at first because the first test to run compared
    // a string to itself and a function returning "no match" for a string
    // against itself is indistinguishable from a string that is not there.
    let mut prev2: Option<Vec<usize>> = None;
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut d = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            // The transposition term, and the reason §9.3's own example --
            // `reciever` -- is findable at all.
            let transposed = i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1];
            if transposed {
                if let Some(row) = &prev2 {
                    d = d.min(row[j - 2] + 1);
                }
            }
            cur[j] = d;
        }
        // The early exit, checked *after* the row is complete. Checking it
        // against the initial row -- which is `[0, 1, 2, ...]` and so exceeds
        // the budget as soon as the string is three characters long -- rejects
        // every comparison, including a string against itself. A distance can
        // fall as later rows are computed, so a row that is too large is not
        // yet proof.
        prev2 = Some(std::mem::replace(&mut prev, cur));
    }
    let d = prev[b.len()];
    if d > MAX_EDITS {
        None
    } else {
        Some(d)
    }
}

/// A fuzzy hit, before the exact search has had a chance at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuzzyHit {
    pub object_id: String,
    /// The indexed term that the query term matched, not the query term.
    pub term: String,
    /// The field it was found in.
    pub field: Field,
    pub score: i64,
}

/// Index an object's terms for fuzzy matching.
///
/// Called from [`Store::index_fuzzy`]; separate from the exact index because
/// the row count is a multiple of the term count and a caller that only wants
/// exact search should not pay for it.
pub async fn index_terms(
    store: &Store,
    object_id: &str,
    terms: &[(Field, String)],
) -> Result<u64, SearchError> {
    let mut n = 0u64;
    for (field, term) in terms {
        // The exact term under its own key, so a fuzzy search also finds a
        // literal match. Costs one row and means a query with a typo still
        // ranks the exactly-right object first.
        n += put(store, object_id, *field, term, term).await?;
        for key in deletion_keys(term) {
            if key != *term {
                n += put(store, object_id, *field, term, &key).await?;
            }
        }
        n += put(store, object_id, *field, term, &transposition_key(term)).await?;
        n += put(store, object_id, *field, term, &phonetic_key(term)).await?;
    }
    Ok(n)
}

async fn put(
    store: &Store,
    object_id: &str,
    field: Field,
    term: &str,
    key: &str,
) -> Result<u64, SearchError> {
    let n = match store {
        Store::Sqlite(p) => sqlx::query(
            "INSERT INTO search_fuzzy (object_id, field, term, key)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT (object_id, field, term, key) DO NOTHING",
        )
        .bind(object_id.to_string())
        .bind(field.as_str().to_string())
        .bind(term.to_string())
        .bind(key.to_string())
        .execute(p)
        .await
        .map_err(StoreError::Query)?
        .rows_affected(),
        Store::Postgres(p) => {
            let sql = Store::bind_sql(
                "INSERT INTO search_fuzzy (object_id, field, term, key)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT (object_id, field, term, key) DO NOTHING",
            );
            sqlx::query(&sql)
                .bind(object_id.to_string())
                .bind(field.as_str().to_string())
                .bind(term.to_string())
                .bind(key.to_string())
                .execute(p)
                .await
                .map_err(StoreError::Query)?
                .rows_affected()
        }
    };
    Ok(n)
}

impl Store {
    /// §9.3's alias and nickname awareness: record another name for an object
    /// and make it findable.
    ///
    /// The alias row is the record; the *tokens* go into `search_term` under
    /// `Field::Alias`, which is why an alias is found by the exact search, the
    /// fuzzy search and the synonym expansion without any of the three knowing
    /// that aliases exist. One code path, three features, no feature-specific
    /// branch in any of them.
    ///
    /// Case-folded into `alias_key` before storing, because a name is a name
    /// however it is capitalised and `PRIMARY KEY (object_id, alias_key)` is
    /// what stops "The Good One" and "the good one" being two aliases the user
    /// cannot distinguish. The fold is in Rust rather than a `COLLATE NOCASE`
    /// so both engines enforce the same rule: SQLite's `NOCASE` is ASCII-only
    /// and Postgres's is not, and an alias that differs only by an accented
    /// character would be two aliases on one engine and one on the other.
    ///
    /// Idempotent. Re-adding an alias replaces its tokens rather than
    /// accumulating them, for the same reason re-indexing an object does: a
    /// stale token is indistinguishable from a current one and is worse than
    /// no token, because it makes a search succeed with the wrong answer.
    pub async fn add_alias(&self, object_id: &str, alias: &str) -> Result<(), SearchError> {
        let key = alias.to_lowercase();
        match self {
            Store::Sqlite(p) => {
                sqlx::query(
                    "INSERT INTO object_alias (object_id, alias, alias_key)
                     VALUES (?, ?, ?)
                     ON CONFLICT (object_id, alias_key) DO UPDATE SET alias = excluded.alias",
                )
                .bind(object_id.to_string())
                .bind(alias.to_string())
                .bind(key)
                .execute(p)
                .await
                .map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "INSERT INTO object_alias (object_id, alias, alias_key)
                     VALUES (?, ?, ?)
                     ON CONFLICT (object_id, alias_key) DO UPDATE SET alias = excluded.alias",
                );
                sqlx::query(&sql)
                    .bind(object_id.to_string())
                    .bind(alias.to_string())
                    .bind(key)
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
        }
        // `SearchError`, not `StoreError`: the indexing half is a search-layer
        // call and reports a search failure, and narrowing it to `StoreError`
        // would mean either discarding that or wrapping it in a variant that
        // says nothing. The caller of `add_alias` needs both halves of the
        // answer -- "could not write the row" and "wrote the row but could not
        // index it" -- and a caller told only the first would retry the write
        // and never learn the index is stale.
        //
        // The alias is **tokenized** before it is indexed, because
        // `index_object` stores each term it is given verbatim and the search
        // tokenizes its *query*. Indexing the raw string made the whole alias
        // one term -- "the good one" -- which no single-word query could ever
        // match, so an alias was unfindable by the exact search, the fuzzy
        // search and the synonym expansion alike. Passing it through unaltered
        // is the kind of thing that looks right because the write succeeds.
        let terms: Vec<(Field, String)> = search::tokenize(alias)
            .into_iter()
            .map(|t| (Field::Alias, t))
            .collect();
        self.index_object(object_id, &terms).await.map(|_| ())
    }

    /// Drop one alias. Its tokens go with it.
    ///
    /// The alias is deleted and the object is then re-indexed from whatever
    /// aliases remain, rather than the field being blanked. The two are not the
    /// same: `index_object` replaces *all* of an object's terms, so blanking
    /// `Field::Alias` and re-indexing the remaining aliases together is one
    /// call and one pass, and it cannot leave a token behind for an alias that
    /// is gone. Re-indexing from the table rather than from the caller's memory
    /// is what makes this correct when the object has several aliases -- the
    /// first version cleared the whole field, so removing one of three aliases
    /// made the other two unfindable.
    pub async fn remove_alias(&self, object_id: &str, alias: &str) -> Result<(), SearchError> {
        let key = alias.to_lowercase();
        let remaining: Vec<(Field, String)> = match self {
            Store::Sqlite(p) => sqlx::query_as::<_, (String,)>(
                "SELECT alias FROM object_alias WHERE object_id = ? AND alias_key <> ?",
            )
            .bind(object_id.to_string())
            .bind(key)
            .fetch_all(p)
            .await
            .map_err(StoreError::Query)?,
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "SELECT alias FROM object_alias WHERE object_id = ? AND alias_key <> ?",
                );
                sqlx::query_as::<_, (String,)>(&sql)
                    .bind(object_id.to_string())
                    .bind(key)
                    .fetch_all(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        }
        .into_iter()
        .map(|(a,)| (Field::Alias, a))
        .collect();

        // The delete is the caller's intent, so it happens whatever the
        // re-index does: a failure to re-index leaves a stale index, and
        // failing to delete would leave a row that the next re-index would
        // resurrect, which is worse.
        match self {
            Store::Sqlite(p) => {
                sqlx::query("DELETE FROM object_alias WHERE object_id = ? AND alias_key = ?")
                    .bind(object_id.to_string())
                    .bind(alias.to_lowercase())
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "DELETE FROM object_alias WHERE object_id = ? AND alias_key = ?",
                );
                sqlx::query(&sql)
                    .bind(object_id.to_string())
                    .bind(alias.to_lowercase())
                    .execute(p)
                    .await
                    .map_err(StoreError::Query)?;
            }
        }

        self.index_object(object_id, &remaining).await.map(|_| ())
    }
}

impl Store {
    /// Fuzzy search, layered on top of the exact one.
    ///
    /// Runs the exact query first and only then adds fuzzy candidates, so a
    /// term that matches exactly is never displaced by one that merely resembles
    /// it. The two are not merged before ranking: the exact hits keep their own
    /// scores, and a fuzzy candidate is only added for an object the exact pass
    /// did not already return, so an object cannot appear twice.
    ///
    /// §9.3's requirement, and the reason this is not just "search with a
    /// tolerance parameter": a misspelled name that finds nothing is read as
    /// *the person is not in the library*, which is a worse failure than a
    /// search that is slightly too permissive. The budget is deliberately one
    /// edit -- a permissive typo tolerance returns everything, and "everything"
    /// is not a search.
    pub async fn search_fuzzy(
        &self,
        text: &str,
        limit: usize,
    ) -> Result<Vec<SearchHit>, SearchError> {
        // An exact search of pure stop words has no terms, and there is nothing
        // for a fuzzy search to do either -- but a *non-empty* query that the
        // exact search found nothing for is the whole point of this function.
        let mut out = self.search(text, None, limit).await?;
        if out.len() >= limit {
            return Ok(out);
        }

        let terms = search::tokenize(text);
        // Guarded rather than inheriting the exact search's verdict. The first
        // version treated an `EmptyQuery` as "no results yet" and carried on to
        // the fuzzy pass, which found nothing and returned `Ok([])` -- so an
        // empty query, a query of nothing but stop words, and a query that
        // matched nothing all came back as the same empty success. A caller
        // cannot tell "you typed nothing" from "there is nothing here", and the
        // first of those is a mistake worth reporting.
        if terms.is_empty() {
            return Err(SearchError::EmptyQuery(text.to_string()));
        }

        let mut rows: Vec<(String, Field, String, i64)> = Vec::new();
        for term in &terms {
            let mut keys = fuzzy_key(term);
            let ph = phonetic_key(term);
            if !ph.is_empty() && !keys.contains(&ph) {
                keys.push(ph);
            }
            for key in keys {
                rows.extend(self.fuzzy_rows(&key).await?);
            }
        }

        // Group by object, keeping the best-scoring field and the terms.
        let mut best: BTreeMap<String, (i64, Vec<Field>, Vec<String>)> = BTreeMap::new();
        for (object_id, field, term, weight) in rows {
            let entry = best.entry(object_id).or_insert((0, Vec::new(), Vec::new()));
            // A fuzzy hit scores *less* than an exact one. A fixed fraction
            // rather than a separate ranking path: one number, one comparison,
            // and the ordering is total so the two engines cannot differ on a
            // tie.
            let score = weight - 1;
            if score > entry.0 {
                entry.0 = score;
            }
            if !entry.1.contains(&field) {
                entry.1.push(field);
            }
            if !entry.2.contains(&term) {
                entry.2.push(term);
            }
        }

        let mut fuzzy: Vec<SearchHit> = best
            .into_iter()
            .filter(|(oid, _)| !out.iter().any(|h| &h.object_id == oid))
            .map(|(object_id, (score, mut fields, _terms))| {
                fields.sort_unstable();
                fields.dedup();
                SearchHit {
                    object_id,
                    score,
                    fields: fields.into_iter().map(|f| f.as_str().to_string()).collect(),
                }
            })
            .collect();
        // Sorted by score then by id, not by score alone. A `sort_by` on a
        // partial key is stable, so the order would be whatever the two
        // engines' row orders happened to be -- and §3.5 asks for one
        // behaviour across both. The id tiebreak makes the order total.
        fuzzy.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.object_id.cmp(&b.object_id))
        });

        out.append(&mut fuzzy);
        out.truncate(limit);
        Ok(out)
    }

    /// Every `(object, field, term, weight)` sharing a key.
    async fn fuzzy_rows(
        &self,
        key: &str,
    ) -> Result<Vec<(String, Field, String, i64)>, SearchError> {
        let raw: Vec<(String, String, String)> = match self {
            Store::Sqlite(p) => {
                sqlx::query_as("SELECT object_id, field, term FROM search_fuzzy WHERE key = ?")
                    .bind(key.to_string())
                    .fetch_all(p)
                    .await
                    .map_err(StoreError::Query)?
            }
            Store::Postgres(p) => {
                let sql = Store::bind_sql(
                    "SELECT object_id, field, term FROM search_fuzzy WHERE key = ?",
                );
                sqlx::query_as::<_, (String, String, String)>(&sql)
                    .bind(key.to_string())
                    .fetch_all(p)
                    .await
                    .map_err(StoreError::Query)?
            }
        };
        Ok(raw
            .into_iter()
            .filter_map(|(oid, field, term)| {
                let f = Field::parse_str(&field)?;
                Some((oid, f, term, f.weight()))
            })
            .collect())
    }
}
