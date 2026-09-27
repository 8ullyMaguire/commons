//! Parsing `Range: bytes=…`, as a pure function with no HTTP in it.
//!
//! T-P5-006 item 9's third part, and the third time in this ticket that a UI
//! feature has turned out to be a server feature first. §10.6 asks for a feed
//! that autoplays and preloads; `<video>` does not ask for a whole file, it
//! asks for ranges, and a route that ignores the header still "works" in a way
//! that is worse than not working at all: a scrub bar that re-downloads the
//! entire file on every seek, over a 4 GB video, on a phone.
//!
//! So the parser is here, separate from the route, for the same reason
//! `feed.ts` is: the parts with an off-by-one in them are the parts you cannot
//! see failing. RFC 9110 §14.1.1 and §14.1.2, and the interesting part is not
//! the happy path — it is what a *malformed* or *unsatisfiable* header must do,
//! because a parser that guesses is a route that serves the wrong bytes to
//! someone who asked for a piece of them.

/// A resolved byte range, HALF-OPEN: `start..end`, so `len` is `end - start`.
///
/// The wire format is inclusive (`bytes=0-99` is bytes 0 through 99) and this
/// type is not, so `end` is the wire's value plus one. The conversion lives
/// here, once, in exactly two places — `resolve` and `content_range` — because
/// a type that is inclusive on the wire and half-open internally is a type
/// where every `len` is off by one, and off-by-one in a byte count is not a
/// test failure you notice: it is a video that skips its last frame, or a
/// client that waits forever for a byte that was never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// EXCLUSIVE. `end == start` is empty; `end == start + 1` is one byte.
    pub end: u64,
}

impl ByteRange {
    /// How many bytes this range covers.
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// What a `Range` header asked for, once checked against a known length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// No usable range: serve the whole body, 200.
    Whole,
    /// One explicit range: 206 with exactly these bytes.
    One(ByteRange),
    /// Several ranges: 206, multipart. **Not implemented** — see the note in
    /// `commons-server`'s media route. It is an enum variant rather than a
    /// rejection so that the one place that has to handle it is a `match` arm
    /// that cannot be forgotten.
    Multiple,
}

/// Why a range header could not be used as written.
///
/// Every variant here is a 416 with a `Content-Range: bytes */len`, and the
/// distinction matters to a client: per RFC 9110 §15.5.17 the reason is not
/// negotiable, and a client that gets `Unsatisfiable` rather than `Malformed`
/// knows to stop asking instead of retrying a header it will never get back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// Syntactically invalid. 416.
    Malformed,
    /// Valid syntax, but not satisfiable against a body of this length. 416.
    Unsatisfiable,
}

/// Resolve a `Range` header against a known body length.
///
/// `len == 0` answers `Unsatisfiable` for *any* range, including a suffix
/// range — which looks wrong until you notice that a suffix range on an empty
/// body asks for "the last N bytes" of nothing. RFC 9110 §14.1.1 requires the
/// 416 in that case rather than a 206 with an empty body, and a client that
/// seeks in an empty file would otherwise wait forever for bytes that cannot
/// arrive.
pub fn resolve(header: Option<&str>, len: u64) -> Result<RangeSpec, RangeError> {
    let Some(raw) = header else {
        return Ok(RangeSpec::Whole);
    };
    let spec = raw.trim();
    // Only `bytes` is a registered range unit for media; `items` exists and
    // means nothing here. A range with an unknown unit is IGNORED per §14.2
    // ("an origin server MUST ignore a Range header field that contains a
    // range unit it does not understand") — that means serve 200, not 416.
    let Some(spec) = spec.strip_prefix("bytes=") else {
        return Ok(RangeSpec::Whole);
    };
    if len == 0 {
        return Err(RangeError::Unsatisfiable);
    }
    if spec.trim().is_empty() {
        return Err(RangeError::Malformed);
    }

    let parts: Vec<&str> = spec.split(',').map(str::trim).collect();
    // The parts must be ascending and non-overlapping (§14.1.1), and this
    // route does not reassemble: a client that sends `0-99,50-149` is asking
    // for a multipart body, and answering 206 with only the first range would
    // hand back a body shorter than the Content-Range claims.
    if parts.len() > 1 {
        return Ok(RangeSpec::Multiple);
    }

    let part = parts[0];
    let (first, last) = match part.split_once('-') {
        Some(pair) => pair,
        // No dash at all: `bytes=abc`. Not a range at all.
        None => return Err(RangeError::Malformed),
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() && last.is_empty() {
        return Err(RangeError::Malformed);
    }

    if first.is_empty() {
        // Suffix: `bytes=-N` is the last N bytes. `-0` is unsatisfiable and
        // NOT malformed -- the syntax is fine, the range is just empty, and
        // §15.5.17 wants the 416.
        let n: u64 = last.parse().map_err(|_| RangeError::Malformed)?;
        if n == 0 {
            return Err(RangeError::Unsatisfiable);
        }
        // Clamp rather than error: `-999999` on a 500-byte file is the whole
        // file, and RFC 9110 §14.1.2 says so explicitly. Treating it as
        // unsatisfiable would break every "give me the tail" request from a
        // client that does not know the length.
        let start = len.saturating_sub(n);
        // Half-open end, like every other branch. The first version wrote
        // `end: len` here — inclusive — which is off by one against the rest
        // of the type, and the test caught it rather than the compiler:
        // `ByteRange::len` is `end - start`, so an inclusive end here reports
        // one byte too many and the route would read one byte past the file.
        return Ok(RangeSpec::One(ByteRange { start, end: len }));
    }

    let start: u64 = first.parse().map_err(|_| RangeError::Malformed)?;
    // `bytes=5-` runs to the end of the body: start 5 through len-1 inclusive,
    // which is `len` half-open.
    let end: u64 = if last.is_empty() {
        len
    } else {
        let requested: u64 = last.parse().map_err(|_| RangeError::Malformed)?;
        if requested < start {
            return Err(RangeError::Unsatisfiable);
        }
        // A last-byte-pos past the end of the body is CLAMPED, not rejected
        // (§14.1.2: "If the last-byte-pos value is greater than or equal to the
        // length of the selected representation data, the range is expressed as
        // the remainder of the representation data"). A client asking for
        // 0-999999 of a 500-byte file wants the file.
        // Wire end is inclusive, so the largest legal value is `len - 1`, and
        // the stored end is one past it.
        requested.min(len - 1) + 1
    };
    if start >= len {
        return Err(RangeError::Unsatisfiable);
    }
    Ok(RangeSpec::One(ByteRange { start, end }))
}

/// The `Content-Range` value for a 206, per §14.4.
pub fn content_range(range: ByteRange, len: u64) -> String {
    // The `+ 1` is the inverse of the conversion in `resolve`, and it is the
    // only place the wire's inclusive form is written. `end` is exclusive
    // internally, so the last byte served is `end - 1`.
    format!("bytes {}-{}/{}", range.start, range.end - 1, len)
}

/// The `Accept-Ranges` value every media response carries.
///
/// Sent even on a 200. A client that cannot see that the server supports
/// ranges will not send `Range` at all, and then never seeks — so a 200 with
/// no `Accept-Ranges` is a scrub bar that silently does nothing.
pub const ACCEPT_RANGES: &str = "bytes";
