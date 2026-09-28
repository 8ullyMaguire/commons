//! Building a command line for an external player, as an argv array.
//!
//! # Why this module performs no I/O
//!
//! It is tempting to make this a route that *launches* mpv. Do not. A function
//! that spawns a process is a function you cannot test, and the ticket's
//! accept criterion is "the argv array equals an expected array" — which is
//! only assertable if the array is returned rather than executed. The caller
//! runs it; this module only decides what to run.
//!
//! # Why argv and never a shell string
//!
//! A media title is data. `Some Song; rm -rf ~` is a real filename and a real
//! scraped title, and the moment it is interpolated into `sh -c "mpv ..."` it
//! is code. So there is no quoting logic here at all: no escaping, no
//! backslashes, no "quote if it contains a space". `std::process::Command`
//! takes argv and does not invoke a shell, which is the whole protection. An
//! implementation that quotes is an implementation that will be wrong on
//! Windows and wrong about `$`, backticks and newlines on POSIX.

use std::ffi::OsString;

/// The players the spec names (§11.3): mpv, VLC, Jellyfin, Plex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalPlayer {
    Mpv,
    Vlc,
    /// Jellyfin and Plex are HTTP clients, not local binaries: they are
    /// handed a URL, and "resume position and injected metadata" rides in the
    /// query string. Modelling them as a player with a different argument
    /// shape is more honest than modelling them as mpv with a different path.
    Jellyfin,
    Plex,
}

/// What is being handed over.
#[derive(Debug, Clone)]
pub struct Handoff {
    /// The media URL the player opens. A URL, never a filesystem path: a path
    /// handed to a remote Jellyfin client is meaningless, and building one is
    /// the difference between "send to my TV" and "send to the server that
    /// already has it".
    pub url: String,
    pub title: String,
    /// Opaque metadata to inject, as key/value pairs. Ordered, because a
    /// command line is ordered and a caller comparing two of them needs a
    /// stable answer.
    pub metadata: Vec<(String, String)>,
    /// Where to start, in milliseconds. `None` means "from the beginning",
    /// which is a different claim from "at 0 ms" and is passed differently: a
    /// seek to zero overwrites the player's own last-position memory, and
    /// "start at the beginning" does not.
    pub resume_ms: Option<i32>,
    /// Milliseconds of lead-in before the resume point. Some players seek
    /// slightly late and clip the first word; a small negative offset is the
    /// standard fix and it belongs here so every player gets the same one.
    pub lead_in_ms: i32,
}

impl Handoff {
    /// A handoff with no resume and no metadata, which is most of them.
    pub fn new(url: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            title: title.into(),
            metadata: Vec::new(),
            resume_ms: None,
            lead_in_ms: 0,
        }
    }

    /// With metadata pairs, in order.
    pub fn with_metadata(
        mut self,
        pairs: impl IntoIterator<Item = (&'static str, String)>,
    ) -> Self {
        self.metadata = pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        self
    }

    /// Resume `ms` into the media, with `lead_in_ms` of lead-in.
    pub fn resuming_at(mut self, ms: i32, lead_in_ms: i32) -> Self {
        self.resume_ms = Some(ms);
        self.lead_in_ms = lead_in_ms;
        self
    }

    /// The resume position in milliseconds, after lead-in and never negative.
    ///
    /// A negative result is clamped to zero rather than passed through: a
    /// player handed `--start -2.000` either errors or, worse, seeks relative
    /// to the end of the media. "Two seconds in" and "two seconds before the
    /// start" are not the same request.
    fn effective_start_ms(&self) -> i32 {
        match self.resume_ms {
            Some(ms) => (ms - self.lead_in_ms).max(0),
            None => 0,
        }
    }
}

/// The command line, as argv.
///
/// Each arm is complete and pushes its own URL. A shared tail line after the
/// `match` would look uniform and would push a SECOND, unadorned URL for the
/// two URL clients — one with no resume and no title, which opens perfectly
/// and silently ignores everything the caller asked for.
pub fn command_line(player: ExternalPlayer, h: &Handoff) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    match player {
        ExternalPlayer::Mpv => {
            argv.push("mpv".into());
            // `--force-media-title` rather than a positional title: a
            // positional argument is a PATH, and a title that happens to look
            // like `/usr/bin/whatever` would be opened as one.
            argv.push("--force-media-title".into());
            argv.push(h.title.as_str().into());
            for (k, v) in &h.metadata {
                argv.push(format!("--{k}={v}").into());
            }
            if h.resume_ms.is_some() {
                // mpv's `--start` is seconds with a useful fractional part.
                // The lead-in already happened in `effective_start_ms`, so
                // this is a plain conversion and not a subtraction.
                argv.push("--start".into());
                argv.push(format!("{:.3}", h.effective_start_ms() as f64 / 1000.0).into());
            }
            argv.push(h.url.as_str().into());
        }
        ExternalPlayer::Vlc => {
            argv.push("vlc".into());
            if h.resume_ms.is_some() {
                // VLC's is `--start-time` in SECONDS, and unlike mpv it does
                // not use a fraction usefully, so it is rounded to the nearest
                // second.
                argv.push("--start-time".into());
                argv.push(format!("{:.0}", h.effective_start_ms() as f64 / 1000.0).into());
            }
            // VLC takes metadata as `--meta-<key> <value>` pairs, not as one
            // `--key=value` argument: a value containing `=` would otherwise
            // be split at the wrong place.
            for (k, v) in &h.metadata {
                argv.push(format!("--meta-{k}").into());
                argv.push(v.as_str().into());
            }
            argv.push(h.url.as_str().into());
        }
        ExternalPlayer::Jellyfin | ExternalPlayer::Plex => {
            // Both are URL clients. The resume rides in the query string and
            // the title in the FRAGMENT, which is not an accident: a fragment
            // is not sent to the server, so a title containing `&`, `#` or
            // `?` cannot corrupt the request. Putting the title in the query
            // string would be a URL-injection bug wearing a metadata feature.
            let mut url = h.url.clone();
            if h.resume_ms.is_some() {
                let sep = if url.contains('?') { '&' } else { '?' };
                url.push_str(&format!(
                    "{sep}startTimeTicks={}",
                    ticks(h.effective_start_ms())
                ));
            }
            let sep = if url.contains('#') { '&' } else { '#' };
            url.push_str(&format!("{sep}title={}", percent_encode(&h.title)));
            argv.push(url.into());
        }
    }
    argv
}

/// 100-nanosecond intervals, which is what Jellyfin's `startTimeTicks` counts.
///
/// It is NOT a Unix timestamp: sending seconds-since-epoch there produces a
/// position in 1970 and a player that seeks to the beginning, which looks
/// exactly like "resume is broken" and is not.
///
/// An offset rather than an absolute clock reading, so a handoff computed at
/// 10:00 and launched at 10:05 seeks to the same place.
fn ticks(ms: i32) -> i64 {
    (ms as i64) * 10_000
}

/// Percent-encode everything outside the unreserved set.
///
/// Written out rather than pulled in as a dependency: it is eight lines and it
/// has to be exactly right, and a URL-encoding crate is a transitive
/// dependency added for one function.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The URL-clients' argument count is the one property a "shared tail"
    /// refactor breaks silently, and it is the one that made this module's
    /// first draft wrong. Asserted here as well as in the integration test so
    /// a `cargo test --lib` alone catches it.
    #[test]
    fn a_url_client_gets_exactly_one_argument() {
        let h = Handoff::new("http://h/x", "A title").resuming_at(5_000, 0);
        for player in [ExternalPlayer::Jellyfin, ExternalPlayer::Plex] {
            let argv = command_line(player, &h);
            assert_eq!(argv.len(), 1, "{player:?} pushed {argv:?}");
            assert!(argv[0].to_string_lossy().contains("startTimeTicks="));
            assert!(argv[0].to_string_lossy().contains("title="));
        }
    }

    /// A local player gets binary, flags, and the URL — never a bare title
    /// that `mpv` would open as a path.
    #[test]
    fn no_resume_position_means_no_seek_flag() {
        let h = Handoff::new("http://h/x", "T");
        let argv = command_line(ExternalPlayer::Mpv, &h);
        assert!(
            !argv.iter().any(|a| a == "--start"),
            "a seek to 0 overwrites the player's own memory: {argv:?}"
        );
        assert_eq!(argv.last().unwrap(), &OsString::from("http://h/x"));
    }

    #[test]
    fn a_lead_in_past_the_start_clamps_to_zero_rather_than_going_negative() {
        // "Two seconds in" and "two seconds before the start" are not the
        // same request, and `--start -2.000` is either an error or a seek
        // relative to the END of the media, depending on the player.
        let h = Handoff::new("http://h/x", "T").resuming_at(500, 2_000);
        let argv = command_line(ExternalPlayer::Mpv, &h);
        // The seek value is the argument AFTER `--start`, not the last one --
        // the last argument is the URL, so reading `argv.last()` here is a
        // test that would pass for the wrong reason if the shape ever changed.
        let at = argv
            .iter()
            .position(|a| a == "--start")
            .expect("a --start flag, since a resume was asked for");
        assert_eq!(argv[at + 1], OsString::from("0.000"), "{argv:?}");
        // Only the SEEK VALUE is checked for a leading minus. Checking every
        // argument fails on `--force-media-title`, which correctly starts with
        // one -- a check broad enough to be wrong is a check that gets
        // weakened until it checks nothing.
        assert!(
            !argv[at + 1].to_string_lossy().starts_with('-'),
            "no negative offset reaches a player: {argv:?}"
        );
    }

    #[test]
    fn a_url_that_already_has_a_query_string_gets_an_ampersand() {
        let h = Handoff::new("http://h/x?api_key=k", "T").resuming_at(1_000, 0);
        let argv = command_line(ExternalPlayer::Jellyfin, &h);
        let url = argv[0].to_string_lossy();
        assert_eq!(url.matches('?').count(), 1, "one question mark: {url}");
        assert!(url.contains("?api_key=k&startTimeTicks="), "{url}");
    }

    #[test]
    fn a_title_with_a_question_mark_does_not_add_one_to_the_query() {
        // The title goes in the FRAGMENT precisely so it cannot do this. This
        // URL has no query of its own, so the fragment separator is `#` and
        // the encoded `?` inside the title stays inside the fragment.
        let h = Handoff::new("http://h/x", "Who? What? Why?").resuming_at(0, 0);
        let a = command_line(ExternalPlayer::Plex, &h);
        let url = a[0].to_string_lossy();
        let (query, fragment) = url
            .split_once('#')
            .unwrap_or_else(|| panic!("no fragment at all: {url}"));
        assert_eq!(query, "http://h/x?startTimeTicks=0", "{url}");
        assert_eq!(fragment, "title=Who%3F%20What%3F%20Why%3F", "{url}");
    }
}
