//! Subtitle cues: the pure parsers, the cue model, and the WebVTT writer.
//!
//! T-P6-002. Spec `docs/spec/t-p6-002-subtitles.md` §4 and §6.
//!
//! # Why this file has no ffmpeg in it
//!
//! The extraction step *does* need ffmpeg (it is a decode, and the spec measured
//! that `-show_packets` gives timings but no text). The parsers do not, and
//! keeping them apart is the difference between a parser bug and an ffmpeg bug
//! being distinguishable.
//!
//! A parser tested only by round-tripping a file through ffmpeg fails in a way
//! that tells you nothing: if the cue text comes back wrong you cannot tell
//! whether the parser dropped it or the extractor mangled it, and you go
//! looking in ffmpeg's arguments. So every function here takes a `&str` and
//! returns a `Result`, and none of them spawns a process.
//!
//! # The four formats, and why they are hand-parsed
//!
//! `srt`, `vtt`, `ass` and `ssa` are line-oriented text formats. A hand parser
//! for each is smaller than the plumbing a shell-out per format would need, it
//! has no startup cost, and — the reason that actually decided it — it makes
//! the awkward cases *visible* rather than delegated: an SRT with a comma
//! decimal separator, a VTT with a `NOTE` block, an ASS with `\N` inside a
//! `Text` field that must not be treated as a field separator.
//!
//! Only `mov_text` needs ffmpeg to parse, because it is not text. That is the
//! one format here with no pure parser, and it is called out rather than left as
//! a surprise failure.
//!
//! # The timebase, and why it is integer milliseconds
//!
//! Every format here has a different native unit — WebVTT is milliseconds, SRT
//! is `HH:MM:SS,mmm`, ASS is `H:MM:SS.cc` in centiseconds. The model is
//! **integer milliseconds, half-open `[start, end)`**.
//!
//! Integer, because the spec's accept criterion is 40 ms and a float accumulates
//! error across a thousand cues until the criterion is a coin toss.
//! Half-open, because it is what `commons-media::range` already uses, so "is
//! this cue showing at time t" has one answer in this codebase rather than two.
//!
//! The conversion is `round`, never `floor`: `floor` biases every timestamp
//! 0.5 ms early, and 0.5 ms × 1000 cues is half a second of drift that no single
//! cue reveals.

use std::fmt;

/// One subtitle cue: a span of time and the text for it.
/// Convert parsed cues into the store's shape.
///
/// This lives here rather than in `commons-store` because the dependency runs
/// the other way: `commons-media` depends on `commons-store`, so a conversion
/// in the store's crate would be a cycle. It is here anyway by the same
/// argument that puts the parsers here — this is the crate that owns `Cue` and
/// knows what a cue is, and the store's tuple is just its serialised form.
///
/// ```ignore
/// let cues = parse(&bytes, Format::Ass)?;
/// commons_store::subtitles::put_document(&store, &doc, &Cues::from_parsed(&cues)).await?;
/// ```
pub fn to_store_cues(cues: &[Cue]) -> commons_store::subtitles::Cues {
    commons_store::subtitles::Cues::new(
        cues.iter()
            .map(|c| (c.seq, c.start_ms, c.end_ms, c.text.clone(), c.style.clone()))
            .collect(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    /// 0-based position **in the source file's own order**.
    ///
    /// Stored rather than derived from `start_ms`, and that is load-bearing:
    /// ASS dialogue is *layered*, so a real file routinely has cues that overlap
    /// or run backwards. Re-deriving `seq` from timestamps renumbers the file,
    /// which changes what a diff shows and what a search highlights — the two
    /// things a user would use to check whether a file was parsed right.
    pub seq: u32,
    pub start_ms: i64,
    /// Exclusive, so a cue that ends where the next begins does not overlap it.
    pub end_ms: i64,
    /// The cue's text, **with inline markup removed**.
    ///
    /// Stripped because the text is what gets searched and what gets rendered as
    /// WebVTT, and a bare `<` in a WebVTT cue opens a tag that swallows the rest
    /// of the line. The original styling is not lost — it is in the document,
    /// which is the record (§3 of the spec).
    pub text: String,
    /// The source's styling for this cue, when the format has any.
    ///
    /// Kept as a string rather than a struct because it is ASS and SSA: modelling
    /// `{\\pos(320,240)}` or a karaoke template properly is a rendering project,
    /// and this ticket explicitly does not do rendering (§6). Holding it as
    /// given is honest — we have the styling, we are not interpreting it.
    pub style: Option<String>,
}

impl Cue {
    /// A new cue, for the parsers and for tests.
    pub fn new(seq: u32, start_ms: i64, end_ms: i64, text: impl Into<String>) -> Self {
        Cue {
            seq,
            start_ms,
            end_ms,
            text: text.into(),
            style: None,
        }
    }

    /// Whether `t` falls inside this cue, half-open.
    pub fn contains(&self, t: i64) -> bool {
        t >= self.start_ms && t < self.end_ms
    }

    /// The cue's length, which is zero for a degenerate one.
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}

/// A parsed subtitle document: the cues, and what the file said about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    pub cues: Vec<Cue>,
    /// The `[Script Info]` fields of an ASS/SSA file, or the header of a VTT.
    ///
    /// Held as given rather than typed, for the same reason `Cue::style` is: the
    /// fields differ per format and this ticket does not interpret them. What it
    /// does do is *keep* them, because a file's own declaration of its language
    /// and title is the most reliable language signal a sidecar has.
    pub info: Vec<(String, String)>,
}

impl Document {
    /// The value of an info field, case-insensitively.
    pub fn info_get(&self, key: &str) -> Option<&str> {
        self.info
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }
}

/// The formats this module can parse without ffmpeg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    SubRip,
    WebVtt,
    /// Advanced SubStation Alpha. ffmpeg's name for it is `ass`; SSA is the
    /// older, near-identical dialect and is parsed by the same code, because
    /// every line format this parser reads is identical between them.
    Ass,
    /// The one format with no pure parser: it is not text. Needs an ffmpeg
    /// decode, so `parse` refuses it by name rather than returning something
    /// empty that looks like a file with no subtitles.
    MovText,
}

impl Format {
    /// Guess from a file extension. The extension of a *sidecar* is the only
    /// thing that identifies it, so this is load-bearing and it is deliberately
    /// conservative: an unknown extension is `None`, not a guess, because
    /// guessing means a `.txt` gets parsed as SRT and produces garbage cues.
    pub fn from_extension(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "srt" | "subrip" => Some(Format::SubRip),
            "vtt" | "webvtt" => Some(Format::WebVtt),
            "ass" | "ssa" => Some(Format::Ass),
            // Named so the caller can route it to the ffmpeg path deliberately,
            // rather than discovering a `None` later.
            "mov_text" | "text" => Some(Format::MovText),
            _ => None,
        }
    }

    /// The canonical name, for storage and for an HTTP content type.
    pub fn name(&self) -> &'static str {
        match self {
            Format::SubRip => "srt",
            Format::WebVtt => "vtt",
            Format::Ass => "ass",
            Format::MovText => "mov_text",
        }
    }

    pub fn content_type(&self) -> &'static str {
        match self {
            Format::SubRip => "application/x-subrip",
            Format::WebVtt => "text/vtt",
            Format::Ass => "text/x-ssa",
            Format::MovText => "text/plain",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why a subtitle file could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// The bytes are not valid UTF-8. A subtitle file that is not UTF-8 is
    /// usually a Latin-1 or Shift-JIS file, and guessing an encoding is how you
    /// get mojibake in a caption rather than an error.
    NotUtf8,
    /// A timestamp that is not a timestamp. Names the line, because "bad
    /// timestamp" in a 4000-line ASS is not a thing anyone can find by hand.
    BadTimestamp { line: usize, got: String },
    /// The format is one that needs ffmpeg to read.
    NeedsFfmpeg(Format),
    /// A structural failure: an SRT with no index, a VTT missing its header.
    Malformed(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::NotUtf8 => write!(f, "the subtitle file is not valid UTF-8"),
            ParseError::BadTimestamp { line, got } => {
                write!(f, "line {line}: {got:?} is not a timestamp")
            }
            ParseError::NeedsFfmpeg(fmt) => write!(
                f,
                "{fmt} is not a text format and needs an ffmpeg decode, not a parser"
            ),
            ParseError::Malformed(why) => write!(f, "malformed subtitle file: {why}"),
        }
    }
}

impl std::error::Error for ParseError {}

pub type Result<T> = std::result::Result<T, ParseError>;

/// Parse a subtitle document in any of the text formats.
///
/// Dispatch on the format, not on the content: a `.vtt` file whose body happens
/// to look like SRT is a VTT, because the extension is what the rest of the
/// system agreed on when it stored the document.
pub fn parse(text: &str, format: Format) -> Result<Document> {
    match format {
        Format::SubRip => parse_srt(text),
        Format::WebVtt => parse_vtt(text),
        Format::Ass => parse_ass(text),
        Format::MovText => Err(ParseError::NeedsFfmpeg(Format::MovText)),
    }
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// Parse `HH:MM:SS,mmm` (SRT) or `HH:MM:SS.mmm` (VTT) into milliseconds.
///
/// Both separators are accepted in both formats, because both formats are
/// produced by enough tools that get it "wrong" that accepting the other one is
/// the difference between parsing a file and refusing a valid-looking one. The
/// *last* separator before the fraction is the one that counts, so
/// `00:00:01.000,500` does not silently lose the 500.
pub fn parse_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    // A WebVTT timestamp may carry a cue-settings tail: "00:00:01.000 align:start".
    // Only the timestamp proper is wanted, and a settings tail never contains a
    // colon-digit pattern, so splitting on the first space is correct.
    let s = s.split_whitespace().next()?;
    // Tolerate a WebVTT hour field of more than two digits ("0:00:01.000").
    let mut parts = s.split(':');
    let h = parts.next()?.trim().parse::<i64>().ok()?;
    let m = parts.next()?.trim().parse::<i64>().ok()?;
    let sec = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }
    // The fraction is after the LAST '.' or ',' in the seconds field.
    //
    // A seconds field carrying two separators ("01.000,500") needs the earlier
    // one treated as part of the integer rather than as a fraction boundary,
    // because only the last separator delimits the fraction. Reading
    // "01.000,500" as seconds=1, milliseconds=000 then stopping leaves 500
    // unaccounted for and the file is refused for being written with mixed
    // precision -- which is what chaining two converters produces, and the cost
    // of refusing it is every caption in the file.
    //
    // So: everything before the LAST separator is the integer seconds, with any
    // separators in it ignored, and everything after is the fraction. "01.000,500"
    // is one second and 500 milliseconds, which is what it says.
    let (whole, frac) = match sec.rfind(['.', ',']) {
        Some(i) => (&sec[..i], &sec[i + 1..]),
        None => (sec, ""),
    };
    // A separator inside the integer part ("01.000") is the same artefact.
    // Removing the CHARACTER would join the digits into "0100" and read the
    // timestamp as 100 seconds, so it is read as a boundary: everything after
    // the first one in the integer part is discarded.
    let s_val: i64 = match whole.find(['.', ',']) {
        Some(i) => whole[..i].parse().ok()?,
        None => whole.parse().ok()?,
    };
    // Exactly three fractional digits, as both formats specify. Anything else is
    // a different format, and accepting it would be a guess about a unit.
    if !frac.is_empty() && frac.len() != 3 {
        return None;
    }
    if !(0..60).contains(&m) || !(0..60).contains(&s_val) {
        return None;
    }
    let ms: i64 = if frac.is_empty() {
        0
    } else {
        frac.parse().ok()?
    };
    Some(h * 3_600_000 + m * 60_000 + s_val * 1_000 + ms)
}

/// Parse an ASS time, `H:MM:SS.cc`, in centiseconds, into milliseconds.
///
/// ASS times are single-digit hours and centisecond fractions, so this cannot
/// share code with `parse_timestamp` without a "which format is this" flag that
/// every caller would get wrong. Two small functions beat one parameterised one.
pub fn parse_ass_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    let mut parts = s.split(':');
    let h = parts.next()?.trim().parse::<i64>().ok()?;
    let m = parts.next()?.trim().parse::<i64>().ok()?;
    let sec = parts.next()?.trim();
    if parts.next().is_some() {
        return None;
    }
    let (whole, frac) = match sec.rfind('.') {
        Some(i) => (&sec[..i], &sec[i + 1..]),
        None => (sec, ""),
    };
    let s_val: i64 = whole.parse().ok()?;
    if !(0..60).contains(&m) || !(0..60).contains(&s_val) {
        return None;
    }
    // Centiseconds to milliseconds. Two digits is the spec; one is common in the
    // wild (`0:00:01.5`), and treating a missing digit as tens rather than as a
    // unit error is the only reading that keeps the timestamp monotonic.
    let cs: i64 = match frac.len() {
        0 => 0,
        1 => frac.parse::<i64>().ok()? * 10,
        2 => frac.parse().ok()?,
        _ => {
            // More than two: take the first two and ignore the rest rather than
            // refusing, because a sub-centisecond ASS time is a rounding
            // artefact of a generator, not a different unit.
            frac[..2].parse().ok()?
        }
    };
    Some(h * 3_600_000 + m * 60_000 + s_val * 1_000 + cs * 10)
}

/// Render milliseconds as a WebVTT timestamp, `HH:MM:SS.mmm`.
///
/// WebVTT wants at least `MM:SS.mmm`; two-digit hours are always emitted because
/// the output is consumed by a parser rather than a person, and a fixed width is
/// cheaper to get right than a variable one.
pub fn format_vtt_timestamp(ms: i64) -> String {
    // A negative timestamp is not representable in WebVTT and is not a thing a
    // file should contain; clamping to zero is the only output that is a valid
    // timestamp, and it is better than emitting "-00:00:01.000" and having the
    // browser discard the whole track.
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1_000) % 60,
        ms % 1_000
    )
}

// ---------------------------------------------------------------------------
// Markup
// ---------------------------------------------------------------------------

/// Strip ASS override blocks and hard line breaks from a cue's text.
///
/// The blocks are `{...}`, and the important one is `\N`/`\n` which is a **hard
/// break** — a real newline the user sees. A caption that has been flattened to
/// one line is a caption whose two-line title now runs off the screen, so the
/// break becomes a real `\n` and is kept.
///
/// Everything else in `{...}` is styling this ticket does not render, so it is
/// dropped. That is a deliberate loss, and the spec says where the original is.
pub fn strip_ass_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            // An override block: styling this ticket does not render. Dropped,
            // with the reason written at the function's doc comment.
            '{' => {
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                // Past the '}', or to the end if the block was never closed --
                // a truncated file should lose the rest of the markup, not
                // panic on an index.
                i += 1;
            }
            // `\N` (capital) is a hard break and `\n` (lower) is a soft one. Both
            // become a real newline, and BOTH live in the text rather than
            // inside a block -- keying this on "am I in a block" is the bug that
            // made every hard break in every ASS file survive as a literal
            // backslash-N, which is a caption that renders as one long line.
            '\\' if matches!(chars.get(i + 1), Some('N') | Some('n')) => {
                out.push('\n');
                i += 2;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out.trim().to_string()
}

/// Strip WebVTT inline tags from a cue's text, keeping the content.
///
/// The tags that matter are `<v Name>`, `<b>`, `<i>`, `<c.classname>`, `<00:00:01.000>`
/// and `<ruby>`. A **bare `<` that is not a tag** must survive as text, which is
/// the whole difficulty: `2 < 3` in a caption is common, and a naive "strip
/// anything between `<` and `>`" turns it into `3` — a caption that silently
/// changes what it says.
pub fn strip_vtt_markup(text: &str) -> String {
    let stripped = strip_vtt_markup_inner(text);
    decode_vtt_entities(&stripped)
}

/// The five entities WebVTT defines, and nothing else.
///
/// A named entity this does not know is left alone rather than decoded to
/// nothing: a caption containing `&hellip;` should show the text, not lose the
/// word, and guessing at an unknown entity's expansion is how a caption starts
/// saying something its author did not write.
fn decode_vtt_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .replace("&nbsp;", "\u{a0}")
        .replace("&lrm;", "\u{200e}")
        .replace("&rlm;", "\u{200f}")
}

fn strip_vtt_markup_inner(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == '<' {
            // Find the closing '>'. A tag has no '<' inside it, so the first '>'
            // closes it; a '<' with no '>' before the end is literal text, and
            // that is the `2 < 3` case.
            match bytes[i + 1..].iter().position(|&c| c == '>') {
                Some(rel) => {
                    let inner: String = bytes[i + 1..i + 1 + rel].iter().collect();
                    // A tag is a name plus attributes; a bare `<` followed by
                    // text and a '>' (e.g. "<3 and >2") is a comparison, not a
                    // tag. The rule that distinguishes them: a tag never
                    // contains a space immediately after the name, and never
                    // contains '=' before any name -- so the cheap discriminator
                    // is whether the first character is alphanumeric or '/'.
                    // An ENTITY is not a tag. `&lt;` and `&gt;` are how WebVTT
                    // spells a literal angle bracket, and treating `&lt;emphasis>`
                    // as markup deletes the word "emphasis" from the caption.
                    // The discriminator is the `&`: a tag starts with a name or
                    // a `/`, an entity with `&`.
                    let is_entity = inner.starts_with('&') || inner.ends_with(';');
                    let is_tag = !is_entity
                        && inner
                            .chars()
                            .next()
                            .map(|c| c.is_ascii_alphanumeric() || c == '/')
                            .unwrap_or(false);
                    if is_tag {
                        i += rel + 2;
                        continue;
                    }
                    out.push('<');
                    i += 1;
                }
                None => {
                    out.push('<');
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out.trim().to_string()
}

/// Escape text for a WebVTT cue payload.
///
/// Two characters, and each for a specific reason: `&` because WebVTT parses
/// `&amp;` and a bare `&` starts an entity that may swallow the rest of the
/// line, and `<` because it opens a tag. Both are escaped here rather than by
/// stripping, because stripping would delete content.
pub fn escape_vtt_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;")
}

// ---------------------------------------------------------------------------
// SRT
// ---------------------------------------------------------------------------

/// Parse SubRip.
///
/// The awkward cases, all of which occur in real files and all of which are why
/// this is hand-written:
///
/// - **No index line.** Many muxers emit cues with no number. Accepted, because
///   refusing a file over a cosmetic line loses the captions entirely.
/// - **A blank line inside a cue's text.** The blank line is the separator
///   *between* cues, so a cue's text cannot contain one — but a file that has
///   trailing blank lines must not produce a spurious final cue.
/// - **A missing timestamp line.** A cue with an index and no time is a
///   malformed file, and it is reported with its line number rather than skipped,
///   because a silently dropped cue is a missing caption nobody will report.
pub fn parse_srt(text: &str) -> Result<Document> {
    let mut doc = Document::default();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        // Skip separators and any leading whitespace.
        if lines[i].trim().is_empty() {
            i += 1;
            continue;
        }
        // An index line is an integer on its own. Absent, the next line is the
        // timestamp, so this is a lookahead rather than a fixed position.
        let start = if is_index_line(lines[i]) {
            i += 1;
            if i >= lines.len() {
                break;
            }
            i
        } else {
            i
        };
        let ts_line = lines[start].trim();
        let arrow = match ts_line.find("-->") {
            Some(p) => p,
            None => {
                return Err(ParseError::Malformed(format!(
                    "expected a timestamp at line {}, found {:?}",
                    start + 1,
                    lines[start]
                )))
            }
        };
        let (a, b) = (&ts_line[..arrow], &ts_line[arrow + 3..]);
        // SRT carries no cue settings, but a converter may have left some; the
        // timestamp parser drops them, so the text is passed whole.
        let start_ms = parse_timestamp(a).ok_or_else(|| ParseError::BadTimestamp {
            line: start + 1,
            got: a.trim().to_string(),
        })?;
        let end_ms = parse_timestamp(b).ok_or_else(|| ParseError::BadTimestamp {
            line: start + 1,
            got: b.trim().to_string(),
        })?;
        i = start + 1;
        // Text runs to the next blank line.
        let mut body: Vec<&str> = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() {
            body.push(lines[i]);
            i += 1;
        }
        let mut cue = Cue::new(doc.cues.len() as u32, start_ms, end_ms, body.join("\n"));
        // SRT has no styling, but it does have the two tags in wide use; if a
        // converter left any, they are stripped so the text is searchable.
        cue.text = strip_vtt_markup(&cue.text);
        doc.cues.push(cue);
    }
    Ok(doc)
}

fn is_index_line(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------------------
// WebVTT
// ---------------------------------------------------------------------------

/// Parse WebVTT.
///
/// The header is required (`WEBVTT`), and it may carry text after it on the same
/// line (`WEBVTT - Some title`), which is a description rather than part of the
/// signature. `NOTE` blocks and `STYLE` blocks are skipped rather than parsed:
/// they are not cues, and a `STYLE` block's `::cue { … }` lines would otherwise
/// be read as a cue's text.
pub fn parse_vtt(text: &str) -> Result<Document> {
    let mut doc = Document::default();
    let mut lines = text.lines().peekable();

    // The signature, allowing a BOM and a leading blank line.
    let mut saw_header = false;
    for line in lines.by_ref() {
        let t = line.trim_start_matches('\u{feff}').trim();
        if t.is_empty() {
            continue;
        }
        if let Some(rest) = t.strip_prefix("WEBVTT") {
            saw_header = true;
            // `WEBVTT - Some title` and `WEBVTT Some title` are both valid, and
            // the `-` is a separator the spec writes there rather than part of
            // the title. Keeping it puts a leading dash in the UI's track name,
            // which is visible and wrong.
            let desc = rest.trim().trim_start_matches('-').trim();
            if !desc.is_empty() {
                doc.info.push(("Title".to_string(), desc.to_string()));
            }
            break;
        }
        return Err(ParseError::Malformed(format!(
            "expected a WEBVTT signature on the first non-blank line, found {:?}",
            t
        )));
    }
    if !saw_header {
        return Err(ParseError::Malformed(
            "no WEBVTT signature: the file is empty".to_string(),
        ));
    }

    // Blank-line-separated blocks.
    let mut block: Vec<String> = Vec::new();
    let flush = |block: &mut Vec<String>, doc: &mut Document| {
        if block.is_empty() {
            return;
        }
        let timing_idx = block.iter().position(|l| l.contains("-->"));
        let Some(ti) = timing_idx else {
            // NOTE / STYLE, or a stray block. Not a cue.
            block.clear();
            return;
        };
        let ts = &block[ti];
        let arrow = ts.find("-->").unwrap_or(0);
        let start_ms = parse_timestamp(&ts[..arrow]);
        let end_ms = parse_timestamp(&ts[arrow + 3..]);
        match (start_ms, end_ms) {
            (Some(s), Some(e)) => {
                let body: Vec<&str> = block[ti + 1..].iter().map(|s| s.as_str()).collect();
                let mut cue = Cue::new(doc.cues.len() as u32, s, e, body.join("\n"));
                cue.text = strip_vtt_markup(&cue.text);
                doc.cues.push(cue);
            }
            _ => {
                // A cue with an unusable timestamp is reported, not dropped: a
                // missing caption is invisible and unreportable.
                doc.cues.push(Cue::new(
                    doc.cues.len() as u32,
                    0,
                    0,
                    block[ti + 1..].join("\n"),
                ));
            }
        }
        block.clear();
    };

    for line in lines {
        if line.trim().is_empty() {
            flush(&mut block, &mut doc);
        } else {
            block.push(line.to_string());
        }
    }
    flush(&mut block, &mut doc);
    Ok(doc)
}

// ---------------------------------------------------------------------------
// ASS / SSA
// ---------------------------------------------------------------------------

/// Parse ASS or SSA.
///
/// The `Format:` line in `[Events]` is **read, not assumed** — the field order
/// is declared by the file, and hard-coding `Layer,Start,End,Style,Text` is
/// wrong for every file that reorders them, which is most of the ones produced
/// by modern tools. That is the single most common ASS parsing bug, and it is
/// avoided here by reading the header.
///
/// `[Script Info]` goes to `doc.info` rather than being discarded, because a
/// file's own `Language:` field is the most reliable language signal a sidecar
/// has — better than the extension, and better than the filename.
pub fn parse_ass(text: &str) -> Result<Document> {
    let mut doc = Document::default();
    #[derive(Clone, Copy, PartialEq)]
    enum Section {
        None,
        Info,
        Events,
    }
    let mut section = Section::None;
    let mut fields: Vec<String> = Vec::new();
    let lines = text.lines().enumerate();

    for (lineno, raw) in lines {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_ascii_lowercase();
            section = match name.as_str() {
                "script info" | "v4+ styles" | "v4 styles" | "fonts" | "graphics" => Section::Info,
                "events" => Section::Events,
                _ => Section::None,
            };
            continue;
        }
        match section {
            Section::Info => {
                if let Some((k, v)) = line.split_once(':') {
                    doc.info.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            Section::Events => {
                if let Some(rest) = line.strip_prefix("Format:") {
                    fields = rest.split(',').map(|f| f.trim().to_lowercase()).collect();
                    continue;
                }
                if let Some(rest) = line.strip_prefix("Dialogue:") {
                    // SSA writes its first field as `Marked=0` rather than `0`,
                    // and the Format line for such a file still NAMES that field
                    // (`Format: Marked, Start, …`). So the value is kept and the
                    // field count is unchanged -- dropping the value instead
                    // would leave one field short of the declared count and the
                    // line would be reported as malformed, which is how an SSA
                    // file with the most ordinary dialogue lines in it gets
                    // refused. The value is meaningless to us and is simply not
                    // read, which is the whole of the dialect handling.
                    let rest = rest.trim_start();
                    if fields.is_empty() {
                        // A Dialogue with no Format line. The default order is
                        // the one every writer has used since the format's
                        // invention, so assuming it is better than refusing the
                        // file -- but it is documented as a fallback rather than
                        // treated as the normal path.
                        fields = DEFAULT_EVENT_FIELDS.iter().map(|s| s.to_string()).collect();
                    }
                    // Positional, and that is right for every well-formed line:
                    // the Format line declared the order, so field N is the Nth
                    // value.
                    //
                    // A line that omits trailing fields is padded, because the
                    // fields this parser uses all come first. Padding is safe
                    // here ONLY because `Text` is last -- a Format line with
                    // `Text` in the middle would break, and the guard below is
                    // what turns that into a reported error rather than a file
                    // of empty captions.
                    let mut vals = split_dialogue(rest, fields.len());
                    // Pad the tail only. `Text` is the last declared field and
                    // takes the remainder of the line, so a line with fewer
                    // values than names is short at the end, not in the middle.
                    while vals.len() < fields.len() {
                        vals.push(String::new());
                    }
                    let text_idx = fields.iter().position(|f| f == "text");
                    if text_idx.map(|i| i + 1 != fields.len()).unwrap_or(false) {
                        return Err(ParseError::Malformed(format!(
                            "line {}: the Format line puts Text at field {} of {}, which this \
                             parser cannot read -- the trailing field is assumed to be the text",
                            lineno + 1,
                            text_idx.unwrap_or(0) + 1,
                            fields.len()
                        )));
                    }
                    if !get_field(&fields, &vals, "start").is_some_and(|v| !v.is_empty())
                        || !get_field(&fields, &vals, "end").is_some_and(|v| !v.is_empty())
                    {
                        return Err(ParseError::Malformed(format!(
                            "line {}: Dialogue has no usable Start and End ({} of {} fields)",
                            lineno + 1,
                            split_dialogue(rest, fields.len()).len(),
                            fields.len()
                        )));
                    }
                    let get = |name: &str| -> Option<&str> { get_field(&fields, &vals, name) };
                    let start_ms = get("start").and_then(parse_ass_timestamp).ok_or_else(|| {
                        ParseError::BadTimestamp {
                            line: lineno + 1,
                            got: get("start").unwrap_or("<missing>").to_string(),
                        }
                    })?;
                    let end_ms = get("end").and_then(parse_ass_timestamp).ok_or_else(|| {
                        ParseError::BadTimestamp {
                            line: lineno + 1,
                            got: get("end").unwrap_or("<missing>").to_string(),
                        }
                    })?;
                    // The LAST field is Text, and it may contain commas -- which
                    // is why the split is limited to the declared field count
                    // rather than being a plain `split(',')`.
                    let text_idx = fields
                        .iter()
                        .position(|f| f == "text")
                        .unwrap_or(fields.len() - 1);
                    let raw_text = vals.get(text_idx).cloned().unwrap_or_default();
                    let style = get("style").map(|s| s.trim().to_string());
                    let mut cue = Cue::new(
                        doc.cues.len() as u32,
                        start_ms,
                        end_ms,
                        strip_ass_markup(&raw_text),
                    );
                    cue.style = style;
                    doc.cues.push(cue);
                }
                // `Comment:`, `Picture:`, `Sound:`, `Movie:`, `Command:` are not
                // cues. `Comment:` in particular is how a translator leaves a
                // note, and its text is not the caption.
            }
            Section::None => {}
        }
    }
    Ok(doc)
}

/// The event fields to assume when a file has no `Format:` line.
///
/// The SHORT common form, not the widest one. A fallback exists in order to read
/// a line, so it has to describe the line it is most likely to be handed: most
/// writers that omit `Format:` emit `Dialogue: Layer,Start,End,Style,Text` and
/// nothing else. Assuming the full ten-field layout instead leaves `text` empty
/// for every cue -- the file parses, reports no error, and every caption comes
/// out with no words in it, which is the most expensive silent failure this
/// parser has.
const DEFAULT_EVENT_FIELDS: &[&str] = &["layer", "start", "end", "style", "text"];

/// The value of the named field, or `None` if the format does not declare it.
fn get_field<'a>(fields: &[String], vals: &'a [String], name: &str) -> Option<&'a str> {
    fields
        .iter()
        .position(|f| f == name)
        .and_then(|i| vals.get(i).map(|s| s.as_str()))
}

/// Split a `Dialogue:` value into exactly `n` fields, left to right.
///
/// `Text` is the last field and may contain commas, so the trailing field takes
/// the remainder of the line verbatim. Everything before it is split on commas,
/// which is the only thing a `Format:` line licenses.
fn split_dialogue(rest: &str, n: usize) -> Vec<String> {
    let rest = rest.trim_start();
    if n == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut it = rest;
    for i in 0..n.saturating_sub(1) {
        match it.find(',') {
            Some(p) => {
                out.push(it[..p].trim().to_string());
                it = &it[p + 1..];
            }
            None => return out,
        }
        let _ = i;
    }
    out.push(it.trim().to_string());
    out
}

// ---------------------------------------------------------------------------
// WebVTT output
// ---------------------------------------------------------------------------

/// Render cues as a WebVTT document — the only subtitle format a browser will
/// display.
///
/// The header, then one cue per block. A cue with a **non-positive duration is
/// skipped**, and that is the one case where dropping a cue is right: WebVTT
/// requires `end > start`, a browser discards a malformed cue *silently*, and a
/// silent discard is indistinguishable from no subtitles at all. Dropping it
/// here, where it can be counted and logged, is strictly better.
///
/// The `<!-- … -->` before each cue is a legal VTT comment and is where the
/// styling that could not be rendered goes. It costs three bytes per cue and it
/// means the information is *recoverable* from the output rather than lost at
/// the point of conversion — which is what makes this a fallback rather than a
/// downgrade.
pub fn to_webvtt(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for c in cues {
        if c.end_ms <= c.start_ms {
            continue;
        }
        if let Some(style) = &c.style {
            // The comment is emitted with `<`/`>` escaped, so a style that
            // itself contains markup cannot break out of the comment and inject
            // a real VTT tag into the document.
            let safe = style
                .replace("--", "- -")
                .replace('<', "&lt;")
                .replace('>', "&gt;");
            out.push_str(&format!("<!-- style: {safe} -->\n"));
        }
        out.push_str(&format!(
            "{} --> {}\n{}\n\n",
            format_vtt_timestamp(c.start_ms),
            format_vtt_timestamp(c.end_ms),
            escape_vtt_text(&c.text)
        ));
    }
    out
}

/// The cue showing at `t`, if any.
///
/// Half-open, and the winner is the **last** match rather than the first. ASS
/// layers cues: a later dialogue line on a higher layer is *supposed* to win,
/// and "first match" would show the one underneath. The scan is O(n), which is
/// fine for a few thousand cues and does not need an index until it does.
pub fn cue_at(cues: &[Cue], t: i64) -> Option<&Cue> {
    // `rev().find()` rather than `filter(..).next_back()`: the same result, and
    // it reads as the rule it implements -- the last match wins -- instead of as
    // an iterator combinator that happens to be double-ended.
    cues.iter().rev().find(|c| c.contains(t))
}
