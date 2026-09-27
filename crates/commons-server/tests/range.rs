//! `Range` parsing, at the boundaries.
//!
//! The property that matters is not "does `bytes=0-99` work" — it is what
//! happens to `bytes=0-999999` on a 500-byte file, `bytes=-0`, and
//! `bytes=abc`. A parser that guesses on those does not fail loudly; it serves
//! the wrong bytes, or serves a 206 whose body is shorter than its own
//! `Content-Range` claims, and a client that trusts the header hangs waiting
//! for the rest.

use commons_server::range::ByteRange as R;
use commons_server::range::{
    content_range, resolve, ByteRange, RangeError, RangeSpec, ACCEPT_RANGES,
};

const LEN: u64 = 500;

fn one(header: &str, len: u64) -> ByteRange {
    match resolve(Some(header), len).expect("a range") {
        RangeSpec::One(r) => r,
        other => panic!("expected one range, got {other:?}"),
    }
}

#[test]
fn no_header_serves_the_whole_body() {
    assert_eq!(resolve(None, LEN).unwrap(), RangeSpec::Whole);
    // A present-but-whitespace header is still no header.
    assert_eq!(resolve(Some("   "), LEN).unwrap(), RangeSpec::Whole);
}

#[test]
fn an_unknown_range_unit_is_ignored_not_rejected() {
    // RFC 9110 14.2: an origin server MUST IGNORE a range unit it does not
    // understand. So 200, not 416. Getting this backwards breaks every client
    // that probes with `items=0-1` first.
    assert_eq!(resolve(Some("items=0-99"), LEN).unwrap(), RangeSpec::Whole);
}

#[test]
fn an_explicit_range_is_half_open_and_exact() {
    // `bytes=0-99` is INCLUSIVE on the wire and HALF-OPEN internally: the
    // parser stores end = 100 so that `len` is a subtraction. Getting that
    // conversion wrong in either direction is a body one byte short, or one
    // byte past the end of the file.
    let r = one("bytes=0-99", LEN);
    assert_eq!((r.start, r.end), (0, 100));
    assert_eq!(r.len(), 100);
}

#[test]
fn an_open_ended_range_runs_to_the_last_byte() {
    // `bytes=100-` on 500 bytes: 100..500, which is 400 bytes and ends at 499.
    let r = one("bytes=100-", LEN);
    assert_eq!((r.start, r.end), (100, LEN));
    assert_eq!(r.len(), 400);
}

#[test]
fn an_open_ended_range_from_zero_is_the_whole_body_as_a_range() {
    let r = one("bytes=0-", LEN);
    assert_eq!(r, R { start: 0, end: LEN });
    assert_eq!(r.len(), LEN);
}

#[test]
fn a_suffix_range_is_the_last_n_bytes() {
    // `-100` on 500 bytes is the last 100 bytes: 400..500 half-open, so `end`
    // is 500 and NOT 499. The end is EXCLUSIVE everywhere in this type, which
    // is what makes `len` a subtraction rather than `+ 1`.
    let r = one("bytes=-100", LEN);
    assert_eq!((r.start, r.end), (400, LEN));
    assert_eq!(r.len(), 100);
}

#[test]
fn a_suffix_range_longer_than_the_body_is_clamped_not_rejected() {
    // 14.1.2: a suffix longer than the representation is the whole
    // representation. A client that does not know the length sends this, and
    // treating it as 416 breaks seeking on every such client.
    let r = one("bytes=-999999", LEN);
    assert_eq!(r, R { start: 0, end: LEN });
}

#[test]
fn an_end_past_the_body_is_clamped_not_rejected() {
    // Same clause: `0-999999` on 500 bytes is the whole file, not an error.
    let r = one("bytes=0-999999", LEN);
    assert_eq!(r, R { start: 0, end: LEN });
    assert_eq!(r.len(), LEN);
}

#[test]
fn an_end_past_the_body_mid_range_keeps_its_start() {
    // 14.1.2 again: the START is what selects. `400-999999` is 400..500.
    let r = one("bytes=400-999999", LEN);
    assert_eq!((r.start, r.end), (400, LEN));
    assert_eq!(r.len(), 100);
}

#[test]
fn the_last_byte_alone_is_one_byte() {
    // The boundary: `499-499` on a 500-byte body. One byte, not zero and not
    // two. This is the case that an inclusive-length implementation gets wrong.
    // `499-499` inclusive is the last byte, which is `499..500` half-open: ONE
    // byte. An implementation that stores the wire value 499 as `end` reports
    // zero bytes and serves an empty 206.
    let r = one("bytes=499-499", LEN);
    assert_eq!(
        r,
        R {
            start: 499,
            end: LEN
        }
    );
    assert_eq!(r.len(), 1);
    assert!(!r.is_empty());
}

#[test]
fn a_start_past_the_body_is_unsatisfiable() {
    assert_eq!(
        resolve(Some("bytes=500-"), LEN),
        Err(RangeError::Unsatisfiable)
    );
    assert_eq!(
        resolve(Some("bytes=501-600"), LEN),
        Err(RangeError::Unsatisfiable)
    );
}

#[test]
fn an_inverted_range_is_unsatisfiable_not_malformed() {
    // The syntax is fine; the range is simply backwards. 15.5.17 wants
    // Unsatisfiable here, and the distinction is what tells a client to stop
    // asking rather than to re-send a header it will never get back.
    assert_eq!(
        resolve(Some("bytes=300-200"), LEN),
        Err(RangeError::Unsatisfiable)
    );
}

#[test]
fn a_zero_suffix_is_unsatisfiable_not_malformed() {
    // `-0` asks for the last zero bytes. Valid syntax, empty range.
    assert_eq!(
        resolve(Some("bytes=-0"), LEN),
        Err(RangeError::Unsatisfiable)
    );
}

#[test]
fn garbage_is_malformed() {
    for header in [
        "bytes=abc",
        "bytes=",
        "bytes=-",
        "bytes=1-2-3", // only the FIRST dash splits, so this is "1" / "2-3"
        "bytes=1.5-2", // a float is not a byte position
        "bytes=-1.5",
        "bytes=--5",
    ] {
        let got = resolve(Some(header), LEN);
        // `1-2-3` splits at the first dash into "1" and "2-3", and "2-3" does
        // not parse as a u64 -- so it is Malformed. That is the assertion: the
        // parser must not silently treat the tail as something else.
        assert_eq!(
            got,
            Err(RangeError::Malformed),
            "{header} should be malformed, got {got:?}"
        );
    }
}

#[test]
fn an_empty_body_refuses_every_range_including_a_suffix() {
    // Looks wrong until you notice `-100` on an empty body asks for the last
    // 100 bytes of nothing. 15.5.17 requires the 416; a 206 with an empty body
    // makes a seeking client wait for bytes that cannot arrive.
    assert_eq!(resolve(Some("bytes=0-"), 0), Err(RangeError::Unsatisfiable));
    assert_eq!(
        resolve(Some("bytes=-100"), 0),
        Err(RangeError::Unsatisfiable)
    );
    // But no header at all is still a 200 with an empty body: the file is real,
    // it is just empty, and 416 would say it does not exist.
    assert_eq!(resolve(None, 0).unwrap(), RangeSpec::Whole);
}

#[test]
fn several_ranges_are_reported_separately_rather_than_silently_truncated() {
    // The route answers multipart. What matters here is that it is a DIFFERENT
    // variant: answering 206 with only the first range hands back a body
    // shorter than the Content-Range claims, and the client waits forever.
    assert_eq!(
        resolve(Some("bytes=0-99,200-299"), LEN).unwrap(),
        RangeSpec::Multiple
    );
    // Trailing junk after a comma is still multiple, not malformed, so the
    // route does not 416 a header it is going to multipart anyway.
    assert_eq!(
        resolve(Some("bytes=0-99, oops"), LEN).unwrap(),
        RangeSpec::Multiple
    );
}

#[test]
fn whitespace_around_a_single_range_is_tolerated() {
    // `bytes= 0-99 ` appears in the wild from hand-built clients.
    assert_eq!(one("bytes= 0-99 ", LEN), R { start: 0, end: 100 });
    assert_eq!(one("  bytes=0-99", LEN), R { start: 0, end: 100 });
}

#[test]
fn a_very_large_body_does_not_overflow_the_length_arithmetic() {
    // A 9-exabyte file is absurd, but the subtraction in `len` is u64 and a
    // header of `bytes=0-18446744073709551615` (u64::MAX) must clamp rather
    // than wrap to a huge range past the end.
    let r = one("bytes=0-18446744073709551615", LEN);
    assert_eq!(r, R { start: 0, end: LEN });
}

#[test]
fn content_range_matches_the_bytes_actually_served() {
    // The header and the body are written in two places, and if they disagree
    // the client trusts the header. This is the string every 206 carries, and
    // it is written from the INTERNAL half-open range -- so the `- 1` is the
    // only translation between them.
    assert_eq!(
        content_range(R { start: 0, end: 100 }, LEN),
        "bytes 0-99/500"
    );
    assert_eq!(
        content_range(
            R {
                start: 400,
                end: LEN
            },
            LEN
        ),
        "bytes 400-499/500"
    );
    // The boundary that makes the `- 1` visible: end == start + 1 is ONE byte,
    // so the header's last-byte-pos is `start`. A route that wrote `end`
    // directly here would claim two bytes and send one.
    assert_eq!(content_range(R { start: 0, end: 1 }, LEN), "bytes 0-0/500");
    assert_eq!(
        content_range(
            R {
                start: 499,
                end: LEN
            },
            LEN
        ),
        "bytes 499-499/500"
    );
}
#[test]
fn the_accept_ranges_token_is_bytes() {
    // Sent on the 200 as well as the 206, and the reason is worth the
    // assertion: a client that cannot see the token will not send Range at all,
    // and then never seeks.
    assert_eq!(ACCEPT_RANGES, "bytes");
}

#[test]
fn range_len_is_zero_only_when_empty() {
    assert_eq!(R { start: 0, end: 0 }.len(), 0);
    assert!(R { start: 0, end: 0 }.is_empty());
    assert!(!R { start: 0, end: 1 }.is_empty());
}
