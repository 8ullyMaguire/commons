//! T-P6-005 accept criterion A2: the external-player command line is an argv
//! array, and the array EQUALS an expected array.
//!
//! # Why the whole file insists on `assert_eq!` on a `Vec<OsString>`
//!
//! A test that renders argv into a string and checks `contains` passes for a
//! builder that is wrong in every way that matters. It cannot tell the
//! difference between one argument and two, between `--start 81.000` and
//! `--start=81.000`, or between a title that was escaped and a title that was
//! silently dropped. Equality on the array is the only assertion that
//! distinguishes those, and every test here is written so that a plausible
//! wrong implementation fails it.
//!
//! # The one that matters most
//!
//! `a_title_full_of_shell_metacharacters_stays_one_argument`. The module has
//! no quoting logic and never will, because `Command` takes argv and does not
//! invoke a shell. That is a decision, and a decision with a security property
//! gets a test or it gets rewritten by the next person who thinks quoting is
//! the tidier style.

use std::ffi::OsString;

use commons_server::external_player::{command_line, ExternalPlayer, Handoff};

/// Compare against a literal argv, so a test reads as the command a person
/// would type.
fn argv(parts: &[&str]) -> Vec<OsString> {
    parts.iter().map(OsString::from).collect()
}

#[test]
fn an_mpv_command_line_is_exactly_this_argv() {
    let h = Handoff::new(
        "http://127.0.0.1:8096/dlna/did/video_1",
        "A talk about Rust",
    )
    .with_metadata([("artist", "A. Person".to_string())])
    .resuming_at(83_000, 2_000);

    // 83s resume, 2s lead-in, so 81.000. The lead-in is subtracted ONCE, in
    // `effective_start_ms`, and not again at the formatting site.
    assert_eq!(
        command_line(ExternalPlayer::Mpv, &h),
        argv(&[
            "mpv",
            "--force-media-title",
            "A talk about Rust",
            "--artist=A. Person",
            "--start",
            "81.000",
            "http://127.0.0.1:8096/dlna/did/video_1",
        ])
    );
}

#[test]
fn a_title_full_of_shell_metacharacters_stays_one_argument() {
    // If someone "improves" this module by adding `sh -c`, or by quoting, this
    // fails and they find out why.
    let nasty = "'; rm -rf ~; echo $(whoami) `id` \"quoted\" & | < >";
    let h = Handoff::new("http://127.0.0.1:8096/x", nasty);

    let a = command_line(ExternalPlayer::Mpv, &h);
    // FOUR arguments: the binary, the flag, the title, the URL. The count is
    // asserted rather than assumed, because "the title is still one argument"
    // is only meaningful if the argument count is pinned -- an implementation
    // that split the title on whitespace would have the same title text at
    // index 2 and a different length.
    assert_eq!(a.len(), 4, "{a:?}");
    assert_eq!(a[0], OsString::from("mpv"));
    assert_eq!(a[1], OsString::from("--force-media-title"));
    assert_eq!(
        a[2],
        OsString::from(nasty),
        "the title is one whole argument"
    );
    // The dangerous text is still INSIDE that argument, inert.
    assert!(a[2].to_string_lossy().contains("rm -rf"));
    assert!(a[2].to_string_lossy().contains("$(whoami)"));
    // And nothing anywhere in the argv looks like a flag that would run it.
    assert!(
        !a.iter().any(|x| x == "-c" || x == "--eval"),
        "no shell is invoked: {a:?}"
    );
    // The URL is its own argument and is not the title: a builder that
    // concatenated them would open a file called "title http://...".
    assert_eq!(a[3], OsString::from("http://127.0.0.1:8096/x"));
}

#[test]
fn no_resume_position_means_no_seek_flag() {
    // A seek to zero overwrites the player's own last-position memory, so
    // "start at the beginning" and "seek to 0 ms" are different requests and
    // only the first one is what a caller with no resume position means.
    for (player, seek) in [
        (ExternalPlayer::Mpv, "--start"),
        (ExternalPlayer::Vlc, "--start-time"),
    ] {
        let a = command_line(player, &Handoff::new("http://h/x", "T"));
        assert!(
            !a.iter().any(|x| x == seek),
            "{player:?} emitted a seek with no resume position: {a:?}"
        );
    }
    // A URL client likewise omits the parameter rather than sending a zero.
    let u = command_line(ExternalPlayer::Jellyfin, &Handoff::new("http://h/x", "T"));
    assert!(
        !u[0].to_string_lossy().contains("startTimeTicks"),
        "{}",
        u[0].to_string_lossy()
    );
}

#[test]
fn a_resume_position_is_not_rounded_to_nice_numbers() {
    // The store holds a millisecond. Rounding it to the nearest 10 s "to be
    // kind" is a different claim from the one the store holds, and a user who
    // closed a video at 1:23:47 and reopens it at 1:23:40 has been lied to in
    // a way nobody can debug later.
    let h = Handoff::new("http://h/x", "T").resuming_at(83_470, 0);
    let a = command_line(ExternalPlayer::Mpv, &h);
    assert!(
        a.iter().any(|x| x == "83.470"),
        "mpv keeps the fraction: {a:?}"
    );
    // VLC genuinely cannot use a fraction here, and its rounding is the
    // player's limitation rather than ours -- so it is asserted as the
    // documented compromise instead of being quietly equal to mpv's.
    let v = command_line(ExternalPlayer::Vlc, &h);
    assert!(
        v.iter().any(|x| x == "83"),
        "vlc rounds to whole seconds, and says so: {v:?}"
    );
}

#[test]
fn a_playlist_url_keeps_its_existing_query_string() {
    // A second `?` produces a URL with a literal `?` in a parameter name, and
    // every client resolves that differently.
    let h = Handoff::new("http://h/dlna/did/v?id=7", "T").resuming_at(1_000, 0);
    let a = command_line(ExternalPlayer::Jellyfin, &h);
    let url = a[0].to_string_lossy();
    assert_eq!(url.matches('?').count(), 1, "one question mark: {url}");
    assert!(url.contains("?id=7&startTimeTicks=10000000"), "{url}");
}

#[test]
fn a_url_client_gets_one_url_and_nothing_else() {
    // The property a "shared tail" refactor breaks silently, and the one that
    // made this module's first draft wrong: a second, unadorned URL opens
    // fine and ignores the resume and the title entirely.
    let h = Handoff::new("http://h/dlna/did/v", "A title")
        .with_metadata([("artist", "A. Person".to_string())])
        .resuming_at(5_000, 0);
    for player in [ExternalPlayer::Jellyfin, ExternalPlayer::Plex] {
        let a = command_line(player, &h);
        assert_eq!(a.len(), 1, "{player:?} produced {a:?}");
        let url = a[0].to_string_lossy();
        assert!(url.starts_with("http://h/dlna/did/v"), "{url}");
        assert!(url.contains("startTimeTicks="), "{url}");
        assert!(url.contains("title=A%20title"), "{url}");
    }
}

#[test]
fn the_title_rides_in_the_fragment_so_it_cannot_corrupt_the_query() {
    // A `&` in a query-string title would append a parameter; a `#` would
    // truncate everything after it. The fragment is not sent to the server, so
    // neither can happen.
    let h = Handoff::new("http://h/x", "Fish & chips #1? really").resuming_at(0, 0);
    let a = command_line(ExternalPlayer::Jellyfin, &h);
    let url = a[0].to_string_lossy();
    let (query, fragment) = url
        .split_once('#')
        .unwrap_or_else(|| panic!("no fragment: {url}"));
    assert!(
        !query.contains("Fish"),
        "the title is not in the query: {query}"
    );
    assert!(query.contains("startTimeTicks=0"), "{query}");
    assert!(
        fragment.contains("title=Fish%20%26%20chips%20%231%3F%20really"),
        "{fragment}"
    );
}

#[test]
fn every_player_opens_the_url() {
    // A handoff that produces no URL opens nothing. Cheap to check and it is
    // the failure a caller sees as "I clicked play on my TV and nothing
    // happened", with no error anywhere.
    let h = Handoff::new("http://h/dlna/did/v", "T");
    for player in [
        ExternalPlayer::Mpv,
        ExternalPlayer::Vlc,
        ExternalPlayer::Jellyfin,
        ExternalPlayer::Plex,
    ] {
        let a = command_line(player, &h);
        assert!(
            a.iter()
                .any(|x| x.to_string_lossy().contains("/dlna/did/v")),
            "{player:?} never mentions the URL: {a:?}"
        );
    }
}
