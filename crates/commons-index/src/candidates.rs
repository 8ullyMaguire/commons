//! §8.2 — candidate generation. Nine proposers, each a separate function.
//!
//! Where a proposal comes from when no upstream source exists: from local
//! signals and from the model, "each a first-class proposer with its own trust".
//!
//! ## Every candidate says why it exists
//!
//! That is the ticket's actual requirement, and it is not a nicety. §8.2 wants
//! the UI to show "title proposed from filename" beside "title proposed by 4
//! users", so a user can accept a source wholesale or field by field — which is
//! only possible if the user can tell the sources apart. A candidate with an
//! empty justification is one the UI cannot group, cannot filter, and cannot
//! explain, which makes it worse than no candidate at all.
//!
//! So `Candidate.justification` is not optional and is not generated at read
//! time. Migration 0007 exists because the alternative — synthesising the wording
//! from the source when it is read — is a guess about an extractor's output
//! presented to the user as a fact.
//!
//! ## Why every proposer is separate
//!
//! Not style. Each of the nine reads a different signal, has a different failure
//! mode, and a different answer to "when should this stay quiet?". A single
//! `propose()` with a match arm per source is the same code, but the *tests* are
//! not: the ticket asks for a positive and a negative test per proposer, and a
//! negative test is only meaningful if it can switch off exactly one thing.
//! Eight of the nine negative tests here would be untestable if the proposers
//! shared a function, because the fixture that triggers one would trigger the
//! others.
//!
//! ## The negative tests are the point
//!
//! A candidate generator that fires on everything is indistinguishable from one
//! that does not work until somebody reads the output. `an_empty_context_produces_nothing_from_any_proposer`
//! and each `*_ignores_*` / `*_drops_*` test exist to keep that from happening,
//! and three of them caught real over-reach while this was being written: a
//! resolution tag parsed as a title, a whitespace-only container tag proposed as
//! a title, and a tagger output with no confidence proposed anyway.

use commons_core::{FieldProposal, ProposalSource, SubjectType};
use commons_store::db::StoreError;
use commons_store::index;
use commons_store::Store;
use std::collections::BTreeMap;
use uuid::Uuid;

/// What a proposer is being run against.
///
/// Deliberately small. A proposer reads *signals*, not the object, so this does
/// not carry the whole `object` row — a generator that could read the current
/// value would start proposing the value back at itself, and that loop is how a
/// curation system ends up re-proposing settled values forever.
#[derive(Debug, Clone, Default)]
pub struct CandidateContext {
    /// The item being proposed for.
    pub object_id: Uuid,
    /// The file the signals came from. `None` for a proposer whose signal is not
    /// per-file — a peer's value is about the object, not a file of it.
    pub file_id: Option<Uuid>,
    /// The filename, for the `filename` proposer. Its own field rather than read
    /// from `file.path`, because the path is what a *scanner* saw and the name a
    /// *user* recognises, and a candidate that quotes the full path reads as
    /// `title proposed from /library/downloads/tmp3/x.mkv`.
    pub filename: Option<String>,
    /// The peer's display name, for the `peer:<id>` proposer. Supplied by the
    /// caller rather than looked up, so this module does not need to know that
    /// `peer.name` exists and a missing name is a caller's problem to solve.
    pub peer_name: Option<String>,
    /// How far apart two perceptual hashes may be and still be the same item.
    /// 0 means exact equality, which is what a real pHash needs: the whole
    /// point of the hash is that the same image at two sizes has two different
    /// hashes, and a threshold that hides that is a threshold that proposes
    /// near-duplicates as the same clip.
    pub phash_distance: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum CandidateError {
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// One proposed value, with the reason it exists.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub field: String,
    pub value: serde_json::Value,
    pub source: ProposalSource,
    /// A sentence, in the UI's language, naming the signal and — where there is
    /// one — the file, container, model or peer it came from. A user deciding
    /// whether to accept a source needs the origin, not the source *name*.
    pub justification: String,
    /// When the extractor said how sure it is. `None` for a signal that has no
    /// confidence — a phash is a hash, not a guess — and §8.2.1 requires it to be
    /// visible when there is one.
    pub confidence: Option<f64>,
    /// The peer, for a `Peer` candidate. Its own field rather than a
    /// `ProposalSource::Peer(String)` variant, because "which proposer" and
    /// "which instance of it" are different questions: the source enum has a
    /// fixed list that `parse` and every `match` depend on, and a per-instance id
    /// inside it would make that list unbounded.
    pub peer: Option<String>,
}

/// Every source in §8.2's table that is a fixed name, in the table's order.
///
/// `peer:<id>` is absent because it is not a fixed name; `peers()` returns the
/// ones that exist. The test writes §8.2's table out by hand, because a test
/// that iterates this list cannot notice a proposer that was forgotten.
pub const ALL_SOURCES: &[ProposalSource] = &[
    ProposalSource::Filename,
    ProposalSource::Embedded,
    ProposalSource::PhashMatch,
    ProposalSource::Transcript,
    ProposalSource::Caption,
    ProposalSource::MlTagger,
    ProposalSource::MlCaptioner,
    ProposalSource::User,
    ProposalSource::Scraper,
];

/// Tunables. Public because two communities will want two answers about how much
/// silence a proposal deserves, and a threshold hard-coded in a match arm is a
/// threshold nobody can argue with.
#[derive(Debug, Clone, PartialEq)]
pub struct CandidateConfig {
    /// Below this, a model tag or a scrape is not proposed at all.
    ///
    /// This is the single most important number in the file. §8.2.1's promise is
    /// that amateur content gets curated even with no official source — which
    /// means the tagger has to be allowed to propose, and it also means the
    /// tagger has to be allowed to shut up. A tagger that proposes everything
    /// floods the proposal queue with noise that a curator must dismiss one at a
    /// time, and a queue like that gets abandoned.
    pub min_confidence: f64,
    /// The fields any proposer may write. A proposer outside this set is not
    /// filtered — it is impossible, because a proposer only ever names fields
    /// from its own table and a field not in the table is never produced.
    pub known_fields: Vec<String>,
    /// The longest title a filename may propose. A 400-character "title" is not a
    /// title, and a curator cannot render it.
    pub max_title_len: usize,
}

impl Default for CandidateConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.5,
            known_fields: vec![
                "title".to_string(),
                "description".to_string(),
                "date".to_string(),
                "tags".to_string(),
                "studio".to_string(),
            ],
            max_title_len: 200,
        }
    }
}

/// Run one proposer.
pub async fn propose(
    store: &Store,
    ctx: &CandidateContext,
    source: ProposalSource,
) -> Result<Vec<Candidate>, CandidateError> {
    propose_with(store, ctx, source, &CandidateConfig::default()).await
}

/// Run one proposer with an explicit configuration.
pub async fn propose_with(
    store: &Store,
    ctx: &CandidateContext,
    source: ProposalSource,
    config: &CandidateConfig,
) -> Result<Vec<Candidate>, CandidateError> {
    let mut out = match source {
        ProposalSource::Filename => filename(ctx),
        ProposalSource::Embedded => embedded(store, ctx).await?,
        ProposalSource::PhashMatch => phash_match(store, ctx).await?,
        ProposalSource::Transcript => transcript(store, ctx).await?,
        ProposalSource::Caption => caption(store, ctx).await?,
        ProposalSource::MlTagger => ml_tagger(store, ctx, config).await?,
        ProposalSource::MlCaptioner => ml_captioner(store, ctx, config).await?,
        ProposalSource::Scraper => scraper(store, ctx, config).await?,
        // `User` is an act, not a signal. Nothing in a file can make a person
        // propose something, so this arm is empty on purpose — and a test says so,
        // because a `User` arm that produced something would mean a machine could
        // write as a person.
        ProposalSource::User => Vec::new(),
        // A peer value is fetched, not generated, and arrives with its own
        // proposer. `propose_all` handles it.
        ProposalSource::Peer => Vec::new(),
        // `Plugin` is a source in the enum but not a row in §8.2's table: a
        // plugin's proposals are produced by the plugin itself, not by a signal
        // this module knows how to read. The arm is here, and empty, so that
        // adding a proposer to the enum is a compile error here rather than a
        // silent fallthrough — which is what an unmatched variant would be if
        // this function were the only one that had to know.
        ProposalSource::Plugin => Vec::new(),
    };
    out.retain(|c| is_plausible(c, config));
    Ok(out)
}

/// Run every proposer, including one per known peer.
pub async fn propose_all(
    store: &Store,
    ctx: &CandidateContext,
) -> Result<Vec<Candidate>, CandidateError> {
    propose_all_with(store, ctx, &CandidateConfig::default()).await
}

pub async fn propose_all_with(
    store: &Store,
    ctx: &CandidateContext,
    config: &CandidateConfig,
) -> Result<Vec<Candidate>, CandidateError> {
    let mut out = Vec::new();
    for source in ALL_SOURCES {
        out.extend(propose_with(store, ctx, *source, config).await?);
    }
    for peer in peers(store).await? {
        out.extend(peer_propose(store, ctx, &peer, config).await?);
    }
    Ok(out)
}

/// Write every candidate to the store, and say how many were written.
///
/// Idempotent by the existing `field_proposal_uniq_idx`, which covers
/// `(subject_type, subject_id, field, value_json, source, proposer_id)`. A
/// rescan re-proposes the same values and the index makes it a no-op rather than
/// a second row, so `propose_and_store` is safe to run on every scan — which is
/// what the scanner will do.
pub async fn propose_and_store(
    store: &Store,
    ctx: &CandidateContext,
) -> Result<usize, CandidateError> {
    let candidates = propose_all(store, ctx).await?;
    let mut written = 0;
    for c in &candidates {
        let mut p = FieldProposal::new(
            SubjectType::Object,
            ctx.object_id,
            &c.field,
            c.value.to_string(),
            c.source,
        );
        // Never `User`. A candidate generated by a parser, a model or a peer is
        // not a person's opinion, and `proposer_kind` is the field the UI reads
        // to decide whether to show an avatar. Getting this wrong is
        // impersonation, and it is the specific failure §8.2.1 exists to prevent.
        //
        // A peer gets `Peer` rather than `Auto`, which is the whole point of
        // §8.2's table having a `peer:<id>` row: it is a different community's
        // settled value, not an automatic extraction, and the UI says so.
        //
        // The peer is named in `proposer_id` rather than being a variant of
        // `ProposalSource`. `peer:<id>` is a per-instance value, so a variant
        // carrying the id would make the enum's `ALL` list — which
        // `parse` and every `match` over sources depend on — unbounded and
        // unhashable-by-value. "Which proposer" and "which instance of it" are
        // different questions and the schema already has a column for the second.
        p.confidence = c.confidence;
        if let Some(peer) = c.peer.as_deref() {
            p.set_peer_provenance(peer, c.justification.clone());
        } else {
            p.set_provenance(c.source.as_str(), c.justification.clone());
        }
        let (_id, inserted) = index::insert_proposal_if_absent(store, &p).await?;
        if inserted {
            written += 1;
        }
    }
    Ok(written)
}

/// Whether a candidate is worth proposing at all.
///
/// The last gate, and the reason the per-proposer logic can be as direct as it
/// is: a field nobody can store, a title nobody can render, and a value that is
/// empty after trimming are not proposals. A value that fails this test is not
/// filtered out at some later point — it is never written, so it never needs
/// filtering again.
fn is_plausible(c: &Candidate, config: &CandidateConfig) -> bool {
    if !config.known_fields.contains(&c.field) {
        return false;
    }
    if c.justification.trim().is_empty() {
        // Not a nicety: §8.2's whole requirement, and a candidate the UI cannot
        // explain is one a user cannot accept or reject on its merits.
        return false;
    }
    match &c.value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return false;
            }
            if c.field == "title" && trimmed.chars().count() > config.max_title_len {
                return false;
            }
            true
        }
        serde_json::Value::Array(items) => {
            // An empty tag list is not a proposal, and a list of empty strings is
            // worse than one.
            !items.is_empty()
                && items
                    .iter()
                    .all(|i| i.as_str().is_some_and(|s| !s.trim().is_empty()))
        }
        // Numbers, objects and null: a proposer that produces them is a bug, and
        // the honest response is to drop it rather than to store something the
        // schema's field types do not describe.
        _ => false,
    }
}

// ---- 1. filename (stash #2680, #484) ---------------------------------------

/// Parse a title and a date out of a filename.
///
/// This one reads no signals, which is the point: the filename is the only
/// source available for a file that arrived by drag-and-drop with no metadata
/// at all, and it is the source a person can most easily correct.
fn filename(ctx: &CandidateContext) -> Vec<Candidate> {
    let Some(name) = ctx.filename.as_deref() else {
        return Vec::new();
    };
    let stem = name
        .rsplit_once('/')
        .map(|(_, base)| base)
        .unwrap_or(name)
        .rsplit_once('.')
        .map(|(stem, _ext)| stem)
        .unwrap_or(name);
    let stem = stem.replace(['_', '.'], " ");

    let mut out = Vec::new();

    // The date first, because it is the part with a real format, and taking it
    // out of the string is what makes the title parseable. (#484)
    if let Some((rest, date)) = split_date(&stem) {
        if is_plausible_date(date) {
            out.push(Candidate {
                field: "date".to_string(),
                value: serde_json::json!(date),
                source: ProposalSource::Filename,
                justification: format!(
                    "date read from the filename \"{name}\", which carries {date}"
                ),
                confidence: None,
                peer: None,
            });
            if let Some(title) = clean_title(rest) {
                out.push(title_candidate(title, name));
            }
        }
    } else if let Some(title) = clean_title(&stem) {
        out.push(title_candidate(title, name));
    }

    // A studio, when the name is `Studio - Title`. Only proposed if the studio
    // part is not itself a quality tag, which is the negative case that matters:
    // `1080p - Something` is not a studio called "1080p".
    if let Some((studio, _rest)) = stem.split_once(" - ") {
        if let Some(s) = clean_title(studio) {
            out.push(Candidate {
                field: "studio".to_string(),
                value: serde_json::json!(s),
                source: ProposalSource::Filename,
                justification: format!(
                    "studio read from the \"Studio - Title\" shape of the filename \"{name}\""
                ),
                confidence: None,
                peer: None,
            });
        }
    }

    out
}

fn title_candidate(title: String, name: &str) -> Candidate {
    Candidate {
        field: "title".to_string(),
        value: serde_json::json!(title),
        source: ProposalSource::Filename,
        justification: format!("title parsed from the filename \"{name}\""),
        confidence: None,
        peer: None,
    }
}

/// Tokens that are metadata about the *file* and never part of a title.
///
/// This list is the reason `filename_proposes_nothing_from_a_quality_tag` passes,
/// and it is the difference between a usable parser and one that titles every
/// file in a library "1080p". Every entry has appeared in real filenames.
const QUALITY_TOKENS: &[&str] = &[
    "1080p", "720p", "480p", "2160p", "4k", "8k", "uhd", "hdr", "hdr10", "sdr", "x264", "x265",
    "h264", "h265", "hevc", "avc", "aac", "ac3", "dts", "flac", "opus", "mp3", "bluray", "bdrip",
    "brrip", "webrip", "web-dl", "webdl", "hdtv", "dvdrip", "dvdscr", "cam", "ts", "r5", "remux",
    "proper", "repack", "extended", "unrated", "imax",
];

/// Words that are a release group's tag rather than a title.
///
/// Small and explicit, because a "common words" list that grows without bound
/// starts eating real titles, and a title that has had a word removed is worse
/// than no title: it looks right and is wrong.
///
/// These are matched as whole *words*, and a dotted host is handled separately in
/// `is_non_title_word` — `www.example.com` is one whitespace-delimited token, so
/// a list of domain suffixes compared for equality never sees it, and the
/// original version of this proposed "www.example.com" as a title.
const NON_TITLE_WORDS: &[&str] = &["www", "com", "net", "org", "co", "uk", "us", "info", "xyz"];

/// A release tag, however it was punctuated.
///
/// Every non-alphanumeric character is removed, not just the ones at the ends,
/// and that is the whole trick: `trim_matches` leaves `H.264` as `h.264` and
/// `WEB.DL` as `web.dl`, neither of which is in the list, so a release tag written
/// the way release tags are actually written was not recognised as one. Both are
/// real spellings -- scene releases write `H.264` and some groups write `WEB-DL`
/// as `WEB.DL` -- and a filename parser that cannot read them proposes the codec
/// as a title.
///
/// Removing every separator also means a list entry can be written with or without
/// them, so `web-dl` and `webdl` in `QUALITY_TOKENS` are both live; that is
/// harmless, and worth knowing if the list is ever trimmed.
fn is_quality_token(word: &str) -> bool {
    let w: String = word
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    QUALITY_TOKENS.contains(&w.as_str())
}

fn is_non_title_word(word: &str) -> bool {
    let w = word
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '.')
        .to_lowercase();
    if NON_TITLE_WORDS.contains(&w.as_str()) {
        return true;
    }
    // A host: something with a dot whose last label is a domain suffix, and whose
    // first label is a subdomain prefix. `Studio.Name.2021` is not a host — its
    // last label is a year — so the suffix list is what decides.
    let mut labels = w.rsplit('.');
    let last = labels.next().unwrap_or_default();
    let has_dot = w.contains('.');
    has_dot && NON_TITLE_WORDS.contains(&last) && labels.next().is_some_and(|_| true)
}

/// A title worth proposing, or `None`.
///
/// The returns-`None` cases are the negative tests: empty, a bare quality tag, a
/// bare domain, and a token soup with nothing but punctuation.
fn clean_title(raw: &str) -> Option<String> {
    let mut title = raw
        .split_whitespace()
        // Bracketed groups are release metadata in every convention there is.
        // `[1080p][x265]` goes; a bracketed *word* stays, because `Title (2019)`
        // is in parentheses but a film's subtitle is sometimes in brackets.
        .collect::<Vec<_>>()
        .join(" ");
    title = strip_bracket_groups(&title);
    title = title
        .split_whitespace()
        .filter(|w| !is_quality_token(w) && !is_non_title_word(w))
        .collect::<Vec<_>>()
        .join(" ");

    let words: Vec<&str> = title.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    // A "title" of pure punctuation or of a single quality token is not a title.
    let alphanumerics = words
        .iter()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count();
    if alphanumerics == 0 {
        return None;
    }
    if words.iter().all(|w| is_quality_token(w)) {
        return None;
    }
    // Note: for a name that is *only* quality tokens this is redundant with the
    // `is_non_title_word` filtering below, which drops every one of them and
    // leaves nothing. It is kept because the two rules answer different
    // questions -- "these are all release tags" and "this word is a suffix" --
    // and collapsing them into one would make a future token that is in
    // `QUALITY_TOKENS` but not `NON_TITLE_WORDS` silently propose itself as a
    // title. The mutation sweep cannot tell them apart, which is a real gap and
    // the reason `a_quality_token_is_not_a_title` tests `is_quality_token`
    // directly rather than through `clean_title`.
    // Nor is a single letter, a single digit, or a lone two-character fragment.
    //
    // This is the negative test `an_empty_context_produces_nothing_from_any_proposer`
    // found: `a.mkv` proposed the title "a". The earlier rule required *some*
    // alphanumeric, and "a" has one. A parser permissive enough to title every
    // single-letter file in a library is not a parser — it is noise with a
    // confidence attached, and the fix is the same shape as the quality-token
    // rule: require enough substance to be worth a curator's time.
    let letters = title.chars().filter(|c| c.is_alphanumeric()).count();
    if letters < 3 {
        return None;
    }
    // Nor is a content hash. `a3f9c2e1.mkv` and `7f3a9b2c1d4e.mkv` are what a
    // downloader or a camera names a file when it has nothing else, and every
    // one of them has enough letters to pass a length check. A title that is one
    // token of hex is not a title, and a curator reading "title proposed from
    // filename: a3f9c2e1" learns nothing.
    //
    // The test is "one token, and it is all hex digits and a-f, and it is long
    // enough to be a hash rather than a word" — `dead` and `beef` are titles, and
    // `face` is a word that happens to be hex.
    if words.len() == 1 {
        let w = words[0];
        if w.len() >= 6
            && w.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
            && w.chars().filter(|c| c.is_ascii_digit()).count() >= 3
        {
            return None;
        }
    }
    let t = title.trim().to_string();
    if t.is_empty() {
        None
    } else {
        Some(t)
    }
}

/// Remove `[...]` and `{...}` groups, and the trailing `(...)` if it is only a
/// date or a quality marker.
fn strip_bracket_groups(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '[' | '{' => depth += 1,
            ']' | '}' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Split a trailing `(YYYY-MM-DD)` or `[YYYY.MM.DD]` off a stem.
fn split_date(stem: &str) -> Option<(&str, &str)> {
    // Two bracket groups at most, right to left. A release name is usually
    // `Title (2021) [1080p]`, so the date is the *earlier* group and the quality
    // tag is the later one -- which means a search that stopped at the first
    // closing bracket found `[1080p]`, decided it was not a date, and gave up.
    //
    // Brackets before parentheses: a name with both is `Title (2021) [1080p]`,
    // and the later group is the quality tag, so checking it first and giving up
    // on "not a date" is what made this miss the real date.
    for (open, close) in [('[', ']'), ('(', ')')] {
        let Some(end) = stem.rfind(close) else {
            continue;
        };
        let Some(start) = stem[..end].rfind(open) else {
            continue;
        };
        let inner = &stem[start + open.len_utf8()..end];
        if is_plausible_date(inner) {
            return Some((&stem[..start], inner));
        }
    }
    None
}

/// Is this the text inside a date's brackets?
///
/// Three shapes, all real release conventions:
///
/// * a bare year, `(2021)` -- by far the most common, and the one the first
///   version of this function did not accept. It required exactly three
///   `YYYY-MM-DD` parts, so every `(2021)` in every release name was skipped as
///   "not a date" and the most conventional filename shape there is yielded no
///   date at all. Found by `a_date_before_a_quality_tag_is_still_a_date`.
/// * `YYYY-MM` -- a Japanese release, which is dated to the month.
/// * `YYYY-MM-DD` -- the full date.
///
/// Anything else is not a date, and in particular a bare number that is not four
/// digits is not a year: `Title (1080)` is a resolution bracket, not 1080 AD.
fn is_plausible_date(s: &str) -> bool {
    let digits = |p: &str, len: usize| p.len() == len && p.chars().all(|c| c.is_ascii_digit());
    // A year in a range a release could plausibly carry.
    //
    // Without this, `(1080)` is a year -- four digits, in brackets -- and the
    // parser proposes the resolution as a date. A resolution always carries a
    // suffix (`1080p`), but a filename is not obliged to use one, and the four
    // digits of `1080` are indistinguishable from `1985` by shape alone. Range is
    // what tells them apart: film releases in this schema are not set in 1080, and
    // the upper bound is far enough out that no plausible year is excluded.
    //
    // The lower bound is 1888, the year of the earliest film this schema would
    // plausibly hold, rounded up to a decade: anything earlier is a catalogue
    // number, a model number, or a resolution.
    const EARLIEST_YEAR: u32 = 1890;
    const LATEST_YEAR: u32 = 2099;
    let year = |y: &str| {
        digits(y, 4)
            && y.parse()
                .is_ok_and(|v: u32| (EARLIEST_YEAR..=LATEST_YEAR).contains(&v))
    };
    let parts: Vec<&str> = s.split(['-', '.', '/']).collect();
    match parts.as_slice() {
        [y] => year(y),
        [year, month] => {
            digits(year, 4) && digits(month, 2) && (1..=12).contains(&month.parse().unwrap_or(0))
        }
        [year, month, day] => {
            digits(year, 4)
                && (digits(month, 2) || digits(month, 1))
                && digits(day, 2)
                && (1..=12).contains(&month.parse().unwrap_or(0))
                && (1..=31).contains(&day.parse().unwrap_or(0))
        }
        _ => false,
    }
}

// ---- 2. embedded (container tags, EXIF/IPTC; stash #2719) -------------------

/// Container and EXIF/IPTC tags.
///
/// The mapping is a closed table rather than "whatever the tag is called",
/// because a container's tag namespace is open — `x264_core_settings` is a real
/// tag in a real mkv, and a proposer that maps tags to fields by convention
/// proposes `cabac=1` as a description.
async fn embedded(store: &Store, ctx: &CandidateContext) -> Result<Vec<Candidate>, CandidateError> {
    let Some(file) = ctx.file_id else {
        return Ok(Vec::new());
    };
    let signals = signals_for(store, file, "container:").await?;
    let mut out = Vec::new();
    for s in signals {
        let Some(field) = container_field(&s.kind) else {
            continue;
        };
        let Some(value) = usable(&s.value) else {
            continue;
        };
        let origin = s.origin.as_deref().unwrap_or("the container");
        out.push(Candidate {
            field: field.to_string(),
            value: serde_json::json!(value),
            source: ProposalSource::Embedded,
            justification: format!(
                "{field} from the {origin} tag `{}`",
                s.kind.trim_start_matches("container:")
            ),
            confidence: s.confidence,
            peer: None,
        });
    }
    Ok(out)
}

/// The closed set of container tags that map to a field.
///
/// A tag not in here is not a field. This function *is* the negative test for
/// `embedded`, which is why it returns `Option` rather than defaulting.
fn container_field(kind: &str) -> Option<&'static str> {
    match kind
        .trim_start_matches("container:")
        .to_ascii_lowercase()
        .as_str()
    {
        "title" => Some("title"),
        "description" | "description_text" | "comment" => Some("description"),
        "date" | "date_recorded" | "dateuploaded" | "year" => Some("date"),
        "tags" | "keywords" | "subject" => Some("tags"),
        _ => None,
    }
}

// ---- 3. phash_match --------------------------------------------------------

/// A described item with the same perceptual hash (stash: the "same clip under
/// two names" case).
///
/// A *lookup*, not a scan: `phash_match` is the only proposer whose question is
/// "what else looks like this?", so the hash is in its own table with its own
/// index rather than read out of a key/value table.
async fn phash_match(
    store: &Store,
    ctx: &CandidateContext,
) -> Result<Vec<Candidate>, CandidateError> {
    let Some(file) = ctx.file_id else {
        return Ok(Vec::new());
    };
    let mine: Option<(String, String)> =
        sqlx::query_as("SELECT phash, algorithm FROM file_phash WHERE file_id = ?")
            .bind(file.to_string())
            .fetch_optional(store.pool())
            .await
            .map_err(StoreError::Query)?;
    let Some((my_hash, my_algo)) = mine else {
        return Ok(Vec::new());
    };

    // Candidates within the threshold, in the same algorithm. The algorithm is
    // part of the join on purpose: two files with the same 64 bits from different
    // implementations are not the same image, and a matcher that ignores this
    // proposes cross-format nonsense.
    //
    // No `AND phash != ?` here. The earlier version had one, to exclude the
    // file's own object -- but `object_id == ctx.object_id` below already does
    // that, and correctly so, because an object can hold several files and only
    // *this* file's own hash is uninteresting. The `phash !=` filter excluded
    // far more than that: every exact match, which is the case §8.2 names first
    // ("the same perceptual hash as a described item"). Hamming distance zero
    // passed the `<= phash_distance` test and was then thrown away anyway, so a
    // byte-identical re-upload of a described item proposed nothing while a
    // near-identical one did. Found by
    // `phash_match_proposes_from_a_described_item_with_the_same_hash`.
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT object_id, phash FROM object_phash
          WHERE algorithm = ?
          ORDER BY phash",
    )
    .bind(&my_algo)
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;

    let mut out = Vec::new();
    for (object_id, their_hash) in rows {
        if hamming(&my_hash, &their_hash) > ctx.phash_distance {
            continue;
        }
        if object_id == ctx.object_id.to_string() {
            continue;
        }
        // Only a *described* item. §8.2 says "the same perceptual hash as a
        // described item", and an object with no title has nothing to propose —
        // matching it would propose the empty string, which is the proposer's
        // version of a null pointer.
        // The consent filter, on a query that had none. A phash match against a
        // `quarantined` or `denied` object was becoming a *proposal* carrying
        // that object's title -- so a takedown's content leaked back into the
        // curation graph, where it is proposed, weighed, and voted on by people
        // who cannot see the object it came from. The takedown blocked the row
        // and the match route walked straight around it.
        //
        // Joined to `consent_record` and bound to the same tier set the browse
        // query uses, so the two cannot disagree: a tier that is invisible in
        // search is invisible here, which is §14.1's "enforced in the data
        // layer, not the UI" meaning that a caller cannot opt out of it by
        // coming at the data a different way.
        let title: Option<String> = sqlx::query_scalar(
            "SELECT o.title FROM object o
               INNER JOIN consent_record c ON c.object_id = o.id
              WHERE o.id = ? AND c.tier IN (?, ?, ?)",
        )
        .bind(&object_id)
        .bind("self_published")
        .bind("performer_claimed")
        .bind("third_party_permitted")
        .fetch_optional(store.pool())
        .await
        .map_err(StoreError::Query)?;
        let Some(title) = title.and_then(|t| usable(&t)) else {
            continue;
        };
        out.push(Candidate {
            field: "title".to_string(),
            value: serde_json::json!(title),
            source: ProposalSource::PhashMatch,
            justification: format!(
                "title from a described item with the same {my_algo} perceptual hash 
                 (distance {} within a threshold of {})",
                hamming(&my_hash, &their_hash),
                ctx.phash_distance
            ),
            confidence: None,
            peer: None,
        });
        break;
    }
    Ok(out)
}

/// Hamming distance between two hex perceptual hashes.
///
/// Returns `u32::MAX` for hashes of different lengths, which is "infinitely far"
/// and makes the comparison above total: a length mismatch is not a match and
/// must not be allowed to look like one.
fn hamming(a: &str, b: &str) -> u32 {
    if a.len() != b.len() {
        return u32::MAX;
    }
    let mut n = 0u32;
    for (x, y) in a.bytes().zip(b.bytes()) {
        n += (x ^ y).count_ones();
    }
    n
}

// ---- 4. transcript (local ASR; stash #8) -----------------------------------

/// Local ASR: a description from what was said, and keywords from the
/// transcript.
async fn transcript(
    store: &Store,
    ctx: &CandidateContext,
) -> Result<Vec<Candidate>, CandidateError> {
    let Some(file) = ctx.file_id else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();

    let transcript = signals_for(store, file, "asr:transcript").await?;
    if let Some(text) = transcript.iter().find_map(|s| usable(&s.value)) {
        out.push(Candidate {
            field: "description".to_string(),
            value: serde_json::json!(text),
            source: ProposalSource::Transcript,
            justification: format!(
                "description from the {} words transcribed from this file's audio",
                text.split_whitespace().count()
            ),
            confidence: None,
            peer: None,
        });
    }

    let keywords = signals_for(store, file, "asr:keywords").await?;
    let mut tags: Vec<String> = Vec::new();
    for s in &keywords {
        let Some(raw) = usable(&s.value) else {
            continue;
        };
        for k in raw.split(',') {
            if let Some(t) = usable(k) {
                if !tags.contains(&t) {
                    tags.push(t);
                }
            }
        }
    }
    if !tags.is_empty() {
        out.push(Candidate {
            field: "tags".to_string(),
            value: serde_json::json!(tags),
            source: ProposalSource::Transcript,
            justification: format!(
                "tags from the keywords extracted after transcribing this file 
                 ({} keyword{})",
                tags.len(),
                if tags.len() == 1 { "" } else { "s" }
            ),
            confidence: None,
            peer: None,
        });
    }
    Ok(out)
}

// ---- 5. caption (sidecar files) --------------------------------------------

/// Sidecar files next to the media.
async fn caption(store: &Store, ctx: &CandidateContext) -> Result<Vec<Candidate>, CandidateError> {
    let Some(file) = ctx.file_id else {
        return Ok(Vec::new());
    };
    let signals = signals_for(store, file, "sidecar:").await?;
    let mut out = Vec::new();
    for s in signals {
        let Some(field) = sidecar_field(&s.kind) else {
            continue;
        };
        let Some(value) = usable(&s.value) else {
            continue;
        };
        // The sidecar's path is the whole value of a sidecar proposal: it is the
        // file a user opens to fix it, and a justification that says "from a
        // sidecar" sends them looking for it.
        let origin = s.origin.as_deref().unwrap_or("a sidecar file");
        out.push(Candidate {
            field: field.to_string(),
            value: serde_json::json!(value),
            source: ProposalSource::Caption,
            justification: format!("{field} from the sidecar file {origin}"),
            confidence: s.confidence,
            peer: None,
        });
    }
    Ok(out)
}

fn sidecar_field(kind: &str) -> Option<&'static str> {
    match kind
        .trim_start_matches("sidecar:")
        .to_ascii_lowercase()
        .as_str()
    {
        "title" => Some("title"),
        "description" | "plot" | "summary" => Some("description"),
        "date" => Some("date"),
        "tags" => Some("tags"),
        // `plot` is mapped to description, so an `.nfo` full of release metadata
        // and no title proposes nothing — which is the negative test.
        _ => None,
    }
}

// ---- 6. ml:tagger (§8.2.1) -------------------------------------------------

/// Local tag inference: structured tags in the `ml:` namespace, with a
/// confidence floor and the model named.
async fn ml_tagger(
    store: &Store,
    ctx: &CandidateContext,
    config: &CandidateConfig,
) -> Result<Vec<Candidate>, CandidateError> {
    let Some(file) = ctx.file_id else {
        return Ok(Vec::new());
    };
    let signals = signals_for(store, file, "ml:tag").await?;
    let mut tags: Vec<String> = Vec::new();
    let mut confidence = 1.0f64;
    let mut model: Option<String> = None;
    let mut dropped = 0usize;

    for s in &signals {
        let Some(value) = usable(&s.value) else {
            continue;
        };
        // Two reasons to stay quiet, and they are different: no confidence at all
        // means the extractor did not say how sure it is, which is not the same
        // as being unsure; and a confidence below the floor means it said and the
        // answer was no. Both are covered by `ml_tagger_drops_*`.
        let Some(c) = s.confidence else {
            dropped += 1;
            continue;
        };
        if c < config.min_confidence {
            dropped += 1;
            continue;
        }
        if let Some(m) = &s.origin {
            model.get_or_insert_with(|| m.clone());
        }
        confidence = confidence.min(c);
        if !tags.contains(&value) {
            tags.push(value);
        }
    }
    if tags.is_empty() {
        return Ok(Vec::new());
    }
    tags.sort();
    let model = model.unwrap_or_else(|| "an unrecorded model".to_string());
    Ok(vec![Candidate {
        field: "tags".to_string(),
        value: serde_json::json!(tags),
        source: ProposalSource::MlTagger,
        justification: format!(
            "tags inferred locally by {model} over this file's frames, lowest 
             confidence {confidence:.2} against a floor of {:.2}{}",
            config.min_confidence,
            if dropped > 0 {
                format!(", {dropped} lower-confidence tag(s) not proposed")
            } else {
                String::new()
            }
        ),
        confidence: Some(confidence),
        peer: None,
    }])
}

// ---- 7. ml:captioner -------------------------------------------------------

/// Local captioning: a title and a description from what is in the frame.
async fn ml_captioner(
    store: &Store,
    ctx: &CandidateContext,
    config: &CandidateConfig,
) -> Result<Vec<Candidate>, CandidateError> {
    let Some(file) = ctx.file_id else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (signal_prefix, field) in [("ml:caption", "description"), ("ml:title", "title")] {
        let signals = signals_for(store, file, signal_prefix).await?;
        for s in &signals {
            let Some(value) = usable(&s.value) else {
                continue;
            };
            let Some(c) = s.confidence else {
                continue;
            };
            if c < config.min_confidence {
                continue;
            }
            let model = s.origin.as_deref().unwrap_or("an unrecorded model");
            out.push(Candidate {
                field: field.to_string(),
                value: serde_json::json!(value),
                source: ProposalSource::MlCaptioner,
                justification: format!(
                    "{field} inferred locally by {model} from this file's visual 
                     content, confidence {c:.2} against a floor of {:.2}",
                    config.min_confidence
                ),
                confidence: Some(c),
                peer: None,
            });
            // One per field: a second caption from the same model at a lower
            // confidence is not a second candidate, it is a worse one.
            break;
        }
    }
    Ok(out)
}

// ---- 8. peer:<id> (§13) ----------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct PeerRef {
    pub id: Uuid,
    pub name: Option<String>,
}

/// The peers this instance knows.
pub async fn peers(store: &Store) -> Result<Vec<PeerRef>, CandidateError> {
    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, name FROM peer WHERE enabled = 1 ORDER BY id")
            .fetch_all(store.pool())
            .await
            .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, name)| Uuid::parse_str(&id).ok().map(|id| PeerRef { id, name }))
        .collect())
}

/// A peer's settled value for this object.
///
/// §13's promise is that a settled value travels. A peer's value is *evidence*,
/// not an answer: it is a candidate with a `peer:<id>` source, weighed like any
/// other, and the peer's own confidence is recorded separately from ours because
/// collapsing the two would let a peer set our mind.
async fn peer_propose(
    store: &Store,
    ctx: &CandidateContext,
    peer: &PeerRef,
    config: &CandidateConfig,
) -> Result<Vec<Candidate>, CandidateError> {
    let rows: Vec<(String, String, Option<f64>)> = sqlx::query_as(
        "SELECT field, value_json, confidence FROM peer_value
          WHERE peer_id = ? AND subject_id = ?",
    )
    .bind(peer.id.to_string())
    .bind(ctx.object_id.to_string())
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;

    // Owned, because the candidate outlives this borrow. A peer with no name
    // falls back to its id, which is ugly and honest: a proposal from a peer
    // nobody named still has to say which peer it was, and an unpresentable id
    // beats an unattributable value.
    let name = peer.name.clone().unwrap_or_else(|| peer.id.to_string());
    let mut out = Vec::new();
    for (field, raw, confidence) in rows {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        // A peer's confidence is theirs, not ours, so it is reported rather than
        // gated: `min_confidence` is about how sure *we* are that a signal is
        // worth reading, and a peer being sure of a bad value is a problem for
        // §13's trust model, not a reason to hide the value from the user. The
        // value competes on the same footing as any other candidate.
        let _ = config.min_confidence;
        // Built before the struct literal, because a struct literal's fields
        // evaluate in order and `field` is moved by the first one.
        let justification = format!(
            "{field} settled by the peer index {name}{}",
            match confidence {
                Some(c) => format!(", whose own confidence is {c:.2}"),
                None => String::new(),
            }
        );
        out.push(Candidate {
            field,
            value,
            source: ProposalSource::Peer,
            peer: Some(name.clone()),
            justification,
            confidence,
        });
    }
    Ok(out)
}

// ---- 9. scraper ------------------------------------------------------------

/// An upstream source, when one exists.
async fn scraper(
    store: &Store,
    ctx: &CandidateContext,
    config: &CandidateConfig,
) -> Result<Vec<Candidate>, CandidateError> {
    let Some(file) = ctx.file_id else {
        return Ok(Vec::new());
    };
    let signals = signals_for(store, file, "scrape:").await?;
    let mut out = Vec::new();
    for s in signals {
        let Some(value) = usable(&s.value) else {
            continue;
        };
        // Unlike a peer's value, a scrape has no trust of its own — it is
        // whatever some upstream site said — so the confidence floor applies.
        match s.confidence {
            Some(c) if c >= config.min_confidence => {}
            _ => continue,
        }
        let origin = s.origin.as_deref().unwrap_or("an upstream source");
        out.push(Candidate {
            field: s.kind.trim_start_matches("scrape:").to_string(),
            value: serde_json::json!(value),
            source: ProposalSource::Scraper,
            justification: format!(
                "scraped from {origin}, confidence {:.2} against a floor of {:.2}",
                s.confidence.unwrap_or_default(),
                config.min_confidence
            ),
            confidence: s.confidence,
            peer: None,
        });
    }
    Ok(out)
}

// ---- shared ----------------------------------------------------------------

/// One extracted signal, as the proposers read it.
pub struct Signal {
    pub kind: String,
    pub value: String,
    pub origin: Option<String>,
    pub confidence: Option<f64>,
}

/// Every signal on a file whose kind starts with `prefix`.
async fn signals_for(
    store: &Store,
    file: Uuid,
    prefix: &str,
) -> Result<Vec<Signal>, CandidateError> {
    let rows: Vec<(String, String, Option<String>, Option<f64>)> = sqlx::query_as(
        "SELECT kind, value, origin, confidence FROM file_signal
          WHERE file_id = ? AND kind LIKE ? ORDER BY kind",
    )
    .bind(file.to_string())
    .bind(format!("{prefix}%"))
    .fetch_all(store.pool())
    .await
    .map_err(StoreError::Query)?;
    Ok(rows
        .into_iter()
        .map(|(kind, value, origin, confidence)| Signal {
            kind,
            value,
            origin,
            confidence,
        })
        .collect())
}

/// A trimmed value that is not empty, or `None`.
///
/// The single place "blank means nothing" is decided, and it is decided for
/// every proposer. Three of the negative tests exist because a version of this
/// did not exist: a whitespace-only container title and a newline-only sidecar
/// both reached `field_proposal` as a proposal to set the title to nothing.
fn usable(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Group candidates by field, for a UI that wants to show "title: three
/// candidates" rather than a flat list.
///
/// Ordered by field name so the rendering is stable, which a `HashMap` would not
/// be — and a candidate list that reorders between two reads of the same data
/// looks to a user like the system is unstable.
pub fn by_field(candidates: &[Candidate]) -> BTreeMap<String, Vec<&Candidate>> {
    let mut out: BTreeMap<String, Vec<&Candidate>> = BTreeMap::new();
    for c in candidates {
        out.entry(c.field.clone()).or_default().push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quality_tag_is_not_a_title() {
        assert!(clean_title("1080p").is_none());
        assert!(clean_title("[1080p][x265]").is_none());
        assert!(clean_title("x264").is_none());
    }

    #[test]
    fn a_content_hash_is_not_a_title() {
        // The case that produced this rule: a downloader's filename.
        assert!(clean_title("a3f9c2e1").is_none());
        assert!(clean_title("7f3a9b2c1d4e5f60").is_none());
        // Short hex words are words.
        assert_eq!(clean_title("dead").as_deref(), Some("dead"));
        assert_eq!(clean_title("cafebabe").as_deref(), Some("cafebabe"));
    }

    #[test]
    fn a_fragment_is_not_a_title() {
        assert!(clean_title("a").is_none());
        assert!(clean_title("ab").is_none());
        assert!(clean_title("12").is_none());
        // Three letters is the shortest thing this will propose, and a real title
        // that short is rare enough to be worth reviewing rather than accepting.
        assert_eq!(clean_title("abc").as_deref(), Some("abc"));
    }

    #[test]
    fn a_domain_is_not_a_title() {
        assert!(clean_title("www.example.com").is_none());
    }

    #[test]
    fn punctuation_is_not_a_title() {
        assert!(clean_title("---").is_none());
        assert!(clean_title("[]{}").is_none());
    }

    #[test]
    fn a_quality_token_inside_a_title_is_kept_out() {
        assert_eq!(
            clean_title("The Sea 1080p [x265]").as_deref(),
            Some("The Sea"),
            "the quality markers are metadata about the file, not the title"
        );
    }

    #[test]
    fn a_date_is_split_off() {
        assert_eq!(
            split_date("Some Title (2021-06-14)").map(|(r, d)| (r.trim(), d)),
            Some(("Some Title", "2021-06-14"))
        );
        assert_eq!(
            split_date("Some Title [2021.06.14]").map(|(r, d)| (r.trim(), d)),
            Some(("Some Title", "2021.06.14"))
        );
    }

    #[test]
    fn an_implausible_date_is_not_a_date() {
        // A month of 99, and a day of 99.
        assert!(split_date("Some Title (2021-99-99)").is_none());
        // A two-digit year.
        assert!(split_date("Some Title (21-06-14)").is_none());
        // A four-digit number outside any plausible year range: `Title (1080)` is
        // a resolution bracket, and 1080 is not a year. Four digits and a bracket
        // are not enough on their own -- that is what the range check is for.
        assert!(split_date("Some Title (1080)").is_none());
        assert!(split_date("Some Title (0042)").is_none());
        // And something that is not a number.
        assert!(split_date("Some Title (Director's Cut)").is_none());
    }

    /// A bare year *is* a date, and the earlier version of this suite said it was
    /// not.
    ///
    /// `an_implausible_date_is_not_a_date` asserted that `Chapter 12 (2021)` holds
    /// no date, which was only true because `is_plausible_date` demanded a full
    /// `YYYY-MM-DD`. So the suite was pinning the bug rather than the intent:
    /// every release named `(2021)` yielded no date, and the assertion stood there
    /// saying that was correct. The test was not wrong about a *number* not being
    /// a year -- `Chapter 12` is a number -- but `(2021)` is bracketed, which is
    /// the signal that it is a date and not a part number.
    #[test]
    fn a_bare_year_is_a_date() {
        assert_eq!(
            split_date("Chapter 12 (2021)").map(|(r, d)| (r.trim(), d)),
            Some(("Chapter 12", "2021"))
        );
        // The month form, for a Japanese release.
        assert_eq!(
            split_date("Some Title (2021-06)").map(|(r, d)| (r.trim(), d)),
            Some(("Some Title", "2021-06"))
        );
        // A year is not a month: `(2021-13)` is a year and an impossible month.
        assert!(split_date("Some Title (2021-13)").is_none());
    }

    #[test]
    fn hamming_counts_differing_bits() {
        assert_eq!(hamming("ffff0000ffff0000", "ffff0000ffff0000"), 0);
        assert_eq!(hamming("0000ffff0000ffff", "ffff0000ffff0000"), 64);
        // A length mismatch is infinitely far, not zero: two hashes of different
        // widths are not a match, and must never be allowed to look like one.
        assert_eq!(hamming("ffff", "ffff0000ffff0000"), u32::MAX);
    }

    #[test]
    fn a_container_tag_maps_only_to_a_field_it_can_store() {
        assert_eq!(container_field("container:title"), Some("title"));
        assert_eq!(container_field("container:x264_core_settings"), None);
        assert_eq!(container_field("container:ENCODER"), None);
    }

    /// The date in `Title (2021) [1080p]` is found even though a later bracket
    /// group is not a date.
    ///
    /// A parser that stopped at the first closing bracket saw `[1080p]`, found no
    /// date in it, and returned nothing -- so it proposed no date at all for the
    /// single most conventional filename shape there is.
    #[test]
    fn a_date_before_a_quality_tag_is_still_a_date() {
        assert_eq!(split_date("Title (2021) [1080p]"), Some(("Title ", "2021")));
        // The reverse order too, and the bracketed-only form.
        assert_eq!(
            split_date("Title [1080p] (2021)"),
            Some(("Title [1080p] ", "2021"))
        );
        assert_eq!(
            split_date("Title (2021-06-14)"),
            Some(("Title ", "2021-06-14"))
        );
        // A quality tag alone is not a date.
        assert_eq!(split_date("Title [1080p]"), None);
        // And a title with no brackets at all.
        assert_eq!(split_date("Some Title"), None);
    }

    /// `QUALITY_TOKENS` and `NON_TITLE_WORDS` overlap on purpose but are not the
    /// same list, so the predicate is tested on its own. Through `clean_title`
    /// the two rules cancel: a name of only quality tokens is emptied by the
    /// word filter regardless, which is why mutating `is_quality_token`'s caller
    /// killed nothing.
    #[test]
    fn a_quality_token_is_not_a_title() {
        for token in ["1080p", "WEB-DL", "x264", "BluRay", "H.264"] {
            assert!(
                is_quality_token(token),
                "{token} is a release tag and must be recognised as one"
            );
        }
        for word in ["Studio", "Title", "Arrival", "de", "la"] {
            assert!(
                !is_quality_token(word),
                "{word} is a word, not a release tag"
            );
        }
    }

    /// The stripped comparison is deliberate: `web-dl` and `webdl` are the same
    /// tag, and a release name contains whichever one the tagger felt like.
    #[test]
    fn a_quality_token_is_recognised_however_it_is_punctuated() {
        for spelling in ["web-dl", "webdl", "WEB.DL", "web dl", "(web-dl)"] {
            assert!(is_quality_token(spelling), "{spelling} is the web-dl tag");
        }
    }
}
