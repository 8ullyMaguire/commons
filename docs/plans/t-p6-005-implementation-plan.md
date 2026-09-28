# T-P6-005 — Cast, DLNA, external players — implementation plan

**Spec:** `docs/spec/t-p6-005-cast-dlna-external-players.md`
**Written:** 2026-09-28, after `8976fa5` (`phase-7-060-interview-derivation`).

**Status: not started.**

Read the spec first. It carries the five ways this is wrong, and three of them
are not discoverable from the code — you have to know that a DLNA server
advertises itself to the whole network with no way to authenticate, which is
what makes the default-off requirement load-bearing rather than cautious.

---

## Step 1 — the external-player argv builder (no I/O, no network)

Do this first. It has no dependencies, it is pure, and it is the part whose
accept criterion is an exact array comparison — so it is the cheapest
verification in the ticket and it establishes the shape of the test files.

### 1.1 The module

`crates/commons-server/src/external_player.rs`

```rust
//! Building a command line for an external player, as an argv array.
//!
//! # Why this module performs no I/O
//!
//! It is tempting to make this a route that *launches* mpv. Do not. A
//! function that spawns a process is a function you cannot test, and A2 is
//! "the argv array equals an expected array" — which is only assertable if
//! the array is returned rather than executed. The caller runs it; this
//! module only decides what to run.
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
    /// query string. Modelling them as a player with a different `finish`
    /// is more honest than modelling them as mpv with a different path.
    Jellyfin,
    Plex,
}
```

### 1.2 The builder

```rust
/// What is being handed over.
#[derive(Debug, Clone)]
pub struct Handoff {
    /// The media URL the player opens. A URL, never a filesystem path: a
    /// path handed to a remote Jellyfin client is meaningless, and building
    /// one is the difference between "send to my TV" and "send to the server
    /// that already has it".
    pub url: String,
    pub title: String,
    /// Opaque metadata to inject, as key/value pairs. Ordered, because a
    /// command line is ordered and a caller comparing two of them needs a
    /// stable answer.
    pub metadata: Vec<(String, String)>,
    /// Where to start, in milliseconds. `None` means "from the beginning",
    /// which is a different claim from "at 0 ms" and is passed differently.
    pub resume_ms: Option<i32>,
    /// Seconds of lead-in before the resume point. Some players seek
    /// slightly late and clip the first word; a small negative offset is
    /// the standard fix and it belongs in the builder so every player gets
    /// the same one.
    pub lead_in_ms: i32,
}

/// The command line, as argv.
pub fn command_line(player: ExternalPlayer, h: &Handoff) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    match player {
        ExternalPlayer::Mpv => {
            argv.push("mpv".into());
            // `--force-media-title` rather than a positional title: a
            // positional argument is a PATH, and a title that looks like
            // `/usr/bin/whatever` would be opened as one.
            argv.push("--force-media-title".into());
            argv.push(h.title.as_str().into());
            for (k, v) in &h.metadata {
                argv.push(format!("--{k}={v}").into());
            }
            if let Some(ms) = h.resume_ms {
                // mpv's `--start` is seconds with a fractional part, and
                // integer milliseconds divided by 1000 truncates DOWN,
                // which is the safe direction: starting 40 ms early loses
                // nothing, starting 40 ms late clips a word.
                argv.push("--start".into());
                argv.push(format!("{:.3}", (ms - h.lead_in_ms).max(0) as f64 / 1000.0).into());
            }
            argv.push(h.url.as_str().into());
        }
        ExternalPlayer::Vlc => {
            argv.push("vlc".into());
            if let Some(ms) = h.resume_ms {
                // VLC's is `--start-time` in SECONDS, and unlike mpv it does
                // not accept a fraction usefully, so it is rounded to the
                // nearest second and ROUNDED DOWN for the same reason.
                argv.push("--start-time".into());
                argv.push(format!("{:.0}", (ms - h.lead_in_ms).max(0) as f64 / 1000.0).into());
            }
            // VLC takes metadata as `--meta-title` style flags.
            for (k, v) in &h.metadata {
                argv.push(format!("--meta-{k}").into());
                argv.push(v.as_str().into());
            }
            argv.push(h.url.as_str().into());
        }
        ExternalPlayer::Jellyfin | ExternalPlayer::Plex => {
            // Both are URL clients. The resume rides in the query string and
            // the title in the fragment, which is not an accident: a fragment
            // is not sent to the server, so a title containing `&` or `#`
            // cannot corrupt the request. Anything that put the title in the
            // query string would be a URL-injection bug wearing a metadata
            // feature.
            //
            // ONE url, pushed once. The local arm below is what makes this
            // tricky to get right by accident: a shared tail line at the end of
            // the match would look uniform and would push a SECOND, unadorned
            // URL for these two players — a URL with no resume and no title,
            // which opens and appears to work while ignoring everything the
            // caller asked for.
            let mut url = h.url.clone();
            if let Some(ms) = h.resume_ms {
                let sep = if url.contains('?') { '&' } else { '?' };
                url.push_str(&format!("{sep}startTimeTicks={}", ticks(ms)));
            }
            let sep = if url.contains('#') { '&' } else { '#' };
            url.push_str(&format!("{sep}title={}", percent_encode(&h.title)));
            argv.push(url.into());
        }
    }
    // The local players open the URL as their final argument; the URL clients
    // above pushed their own. So the tail is NOT shared -- each arm is
    // complete. Read `argv` at the end of each local arm if you are unsure
    // which is which.
    argv
}
```

### 1.3 The helpers

```rust
/// 100-nanosecond intervals since the epoch, which is what Jellyfin's
/// `startTimeTicks` counts. It is NOT a Unix timestamp: sending seconds-since-
/// epoch there produces a position in 1970 and a player that seeks to the
/// beginning, which looks like "resume is broken".
fn ticks(ms: i32) -> i64 {
    // An offset from a fixed epoch, not from `now`: a handoff computed at
    // 10:00 and launched at 10:05 must seek to the same place.
    (ms as i64) * 10_000
}

/// Percent-encode everything that is not unreserved. Written out rather than
/// pulled in as a dependency: it is nine lines and it has to be exactly
/// right, and a URL-encoding crate is a transitive dependency added for one
/// function.
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
```

### 1.4 The test — A2

`crates/commons-server/tests/external_player_argv.rs`

**The property: the returned array EQUALS an expected array.** A test that
renders the argv into a string and checks `contains` passes for a builder that
is wrong in every way that matters.

```rust
#[test]
fn an_mpv_command_line_is_exactly_this_argv() {
    let h = Handoff {
        url: "http://127.0.0.1:8096/dlna/did/video_1".into(),
        title: "A talk about Rust".into(),
        metadata: vec![("artist".into(), "A. Person".into())],
        resume_ms: Some(83_000),
        lead_in_ms: 2_000,
    };
    assert_eq!(
        command_line(ExternalPlayer::Mpv, &h),
        vec![
            "mpv", "--force-media-title", "A talk about Rust",
            "--artist=A. Person",
            "--start", "81.000",
            "http://127.0.0.1:8096/dlna/did/video_1",
        ]
        .into_iter().map(OsString::from).collect::<Vec<_>>(),
    );
}
```

**And the test that makes the argv claim true — this one is not optional:**

```rust
#[test]
fn a_title_full_of_shell_metacharacters_stays_one_argument() {
    // The reason this module has no quoting logic. If someone "improves" it by
    // adding `sh -c`, or by quoting, this fails and they find out why.
    let h = Handoff {
        url: "http://127.0.0.1:8096/x".into(),
        title: "'; rm -rf ~; echo $(whoami) `id` \"quoted\" & | < >".into(),
        metadata: vec![],
        resume_ms: None,
        lead_in_ms: 0,
    };
    let argv = command_line(ExternalPlayer::Mpv, &h);
    // Still exactly three arguments: the binary, the flag, the title.
    assert_eq!(argv.len(), 3, "{argv:?}");
    assert_eq!(argv[2], OsString::from(h.title), "the title is one argument");
    // And the dangerous parts are still inside it, unexecuted.
    assert!(argv[2].to_string_lossy().contains("rm -rf"));
}
```

Three more, each for a distinct reason:

- `no_resume_position_means_no_seek_flag` — `resume_ms: None` must not emit
  `--start 0.000`. "Start at the beginning" and "seek to 0" are different
  claims, and the second one overwrites the player's own last-position memory.
- `a_resume_position_is_not_rounded_to_nice_numbers` — `resume_ms: Some(83_470)`
  gives `81.470` for mpv, not `81` and not `80`. Rounding is a different claim
  from the one the store holds.
- `a_playlist_url_keeps_its_existing_query_string` — a URL that already has
  `?` must get `&`, not a second `?`.

### 1.5 Verify

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
cargo test -p commons-server --test external_player_argv
```

**Expected:** `test result: ok.` and **at least 5 passed, 0 failed.** Fewer than
5 means the file was truncated — check that every test above is present.

**Commit:** `feat: an external player gets an argv, never a shell string`

---

## Step 2 — the DLNA config block, off by default

Before any responder, because the responder is the part that must not exist by
accident.

### 2.1 The types

`crates/commons-server/src/config.rs`

**Note `FileConfig` carries `#[serde(deny_unknown_fields)]`.** That is
deliberate — it turns a typo'd key into an error instead of a setting that
silently does nothing — and it means `pub dlna: Option<DlnaConfig>` is what
makes `[dlna]` in a TOML file *work at all*. Without the field, a user who
reads the spec, writes the block, restarts, and finds DLNA still off gets a
parse error naming the section. That is the correct outcome, but only because
the field exists.

`DlnaConfig` needs `#[derive(Debug, Clone, PartialEq, Eq, Serialize,
Deserialize)]` and `#[serde(default)]` on its `enabled` field, so a config
file saying only `bind = "..."` means "enabled at the default" — which is
`false` — rather than failing to parse. `Option<DlnaConfig>` in `FileConfig`
is what distinguishes "not mentioned" from "mentioned and false".

```rust
/// DLNA/UPnP, and it is OFF unless someone turns it on.
///
/// Not a default-on feature with a default-on-off-switch. A media server that
/// advertises itself to the LAN on upgrade exposes the whole library to every
/// device on that network, and SSDP has no authentication to gate it on — the
/// `media_path` gate every HTTP route uses has nothing to attach to here.
/// `Dlpna` is spelled "Dlna" in Rust and "dlna" in TOML; the acronym is
/// capitalised because `DlnA` would be worse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlnaConfig {
    pub enabled: bool,
    /// The address the SSDP responder binds. `0.0.0.0:1900` reaches the LAN;
    /// `127.0.0.1:1900` does not. Defaulting to loopback is the difference
    /// between "on, for me" and "on, for the office".
    pub bind: String,
    /// The `LOCATION` advertised in the SSDP reply. It must be a URL a client
    /// can fetch, and it must NOT be `0.0.0.0` — that string means "this
    /// machine" to nobody and fails in a way that looks like a broken TV.
    pub location_base: String,
    /// The friendly name a TV shows. Defaults to the library's name.
    pub friendly_name: String,
}
```

**`FileConfig` gains `pub dlna: Option<DlnaConfig>`** and **`Config` gains
`pub dlna: DlnaConfig`.**

`resolve()` gains, after the `public_base_url` binding:

```rust
// Default OFF, and default to loopback even when someone turns it on. Both
// defaults are the safe direction; `enabled = true` in a config file is a
// decision, and the decision should have to be typed.
let dlna = file
    .and_then(|f| f.dlna.clone())
    .unwrap_or_else(|| DlnaConfig {
        enabled: false,
        bind: "127.0.0.1:1900".to_string(),
        location_base: public_base_url.clone(),
        friendly_name: "commons".to_string(),
    });
```

and the `Ok(Config { … })` gains `dlna,`.

### 2.2 Fix the one construction site

`crates/commons-server/tests/support/mod.rs:81` builds a `Config` by struct
literal. **This is the only one in the workspace** (verified:
`grep -rn "Config {" crates/ | grep -v FileConfig`), so the compiler will point
at exactly one place. Add `dlna: DlnaConfig { … }` with `enabled: false`.

### 2.3 A test that the default is off

In `crates/commons-server/src/config.rs`'s existing `#[cfg(test)] mod tests`
(there is one at line ~320):

```rust
#[test]
fn dlna_is_off_and_loopback_bound_by_default() {
    let c = resolve(&cli(&[]), Some(&FileConfig::default())).unwrap();
    assert!(!c.dlna.enabled, "DLNA must be a decision, not a default");
    assert!(
        c.dlna.bind.starts_with("127.0.0.1:"),
        "and loopback-bound even when enabled: {}",
        c.dlna.bind
    );
    assert!(
        !c.dlna.location_base.contains("0.0.0.0"),
        "a LOCATION of 0.0.0.0 is unroutable: {}",
        c.dlna.location_base
    );
}
```

That last assertion is about a string that only exists if someone edits the
default, which is the point: it makes the mistake a test failure rather than a
field report from someone's TV.

### 2.4 Verify

```sh
cargo test -p commons-server --lib config
```

**Expected:** every config test passes, including the new one.

**Commit:** `feat: DLNA config, off by default and loopback-bound when on`

---

## Step 3 — the SSDP responder

The part that fails silently, which is why A1 is a test and not a manual check.

### 3.1 The search target and the reply

`crates/commons-server/src/dlna.rs`

```rust
//! SSDP discovery and a ContentBrowse responder.
//!
//! # The property under test
//!
//! `A-SEARCH * HTTP/1.1` is multicast UDP. It has no reply address, no
//! authentication, and no error: a responder on the wrong interface, or one
//! that answers on a port the client is not reading, is indistinguishable
//! from no responder at all. So this module is only trustworthy if a test
//! sends a real `M-SEARCH` and reads a real reply, which is A1.
//!
//! # What is served
//!
//! Exactly the objects `media_path(&id, &local_caller())` permits. A
//! ContentBrowse handler that queries the `object` table directly serves a
//! library the caller cannot see, and the 404 gate has nothing to attach to
//! on the SSDP side — the client is on the LAN and has never authenticated.
//! `browse_root` therefore goes through the same gate as the HTTP routes,
//! and `an_absent_object_and_a_denied_object_browse_identically` asserts it.

use std::net::{Ipv4Addr, SocketAddr};
use tokio::net::UdpSocket;

/// The device type a DLNA client searches for. This exact string, because a
/// client that does not recognise the response target does not ask again.
pub const MEDIA_SERVER_DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaServer:1";

/// `ssdp:all` is the discovery catch-all; the MediaServer type is the
/// specific one. Answering only the catch-all is a valid implementation and
/// a worse one: a client that asked for a MediaServer gets a device list.
pub const SEARCH_TARGETS: &[&str] = &["ssdp:all", MEDIA_SERVER_DEVICE_TYPE];
```

### 3.2 The responder

```rust
/// Bind the SSDP socket.
///
/// No `SO_REUSEADDR` here, and that is a deliberate omission rather than an
/// oversight: setting it means either a `socket2` dependency (not present in
/// this workspace) or a libc call (worse). It is also not needed for the
/// tests, which bind port `0` and let the OS choose a free port — no address
/// is ever contended.
///
/// It IS needed in production if two instances share a host, and the correct
/// place to add it is then `tokio::net::UdpSocket::bind_from_std` over a
/// socket built by `socket2`. Recorded here so the next person does not read
/// the absence as a bug and "fix" it by adding a dependency for no test gain.
pub async fn bind_responder(bind: &str) -> std::io::Result<UdpSocket> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let sock = UdpSocket::bind(addr).await?;
    // Broadcast is what lets a reply reach a client that sent its M-SEARCH to
    // the multicast group but is listening on the broadcast address.
    // `let _` because a platform that refuses it is not a reason to fail: the
    // unicast reply path — which is what the test and most real clients use —
    // does not need it.
    let _ = sock.set_broadcast(true);
    Ok(sock)
}

/// Does this datagram ask for something this server answers?
///
/// A testable predicate rather than logic buried in the receive loop, so the
/// "which packets do we reply to" decision can be asserted without a socket.
/// Case-insensitive because `M-SEARCH` arrives as `m-search` from real
/// clients, and a responder that matches case-sensitively is a responder that
/// works against `curl` and not against a television.
pub fn is_search_for(payload: &str, device_type: &str) -> bool {
    let text = payload.to_ascii_uppercase();
    if !text.starts_with("M-SEARCH * HTTP/1.1") {
        return false;
    }
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ST:") {
            let v = v.trim();
            if v == "SSDP:ALL" || v == device_type.to_ascii_uppercase() {
                return true;
            }
        }
    }
    false
}

/// The `200 OK` for an `M-SEARCH`, including the headers a client requires
/// before it will fetch anything.
///
/// `CACHE-CONTROL: max-age=1800` is not decoration: without it some clients
/// re-search immediately, so a server that answers correctly can still look
/// like it is "flashing" in a UI. `EXT:` is required by the spec and clients
/// that check for it will not fall back gracefully.
pub fn search_reply(usn: &str, location: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\n\
         CACHE-CONTROL: max-age=1800\r\n\
         EXT:\r\n\
         LOCATION: {location}\r\n\
         SERVER: commons/1.0 UPnP/1.0\r\n\
         ST: {MEDIA_SERVER_DEVICE_TYPE}\r\n\
         USN: {usn}::upnp:rootdevice\r\n\
         \r\n"
    )
}
```

**A note on `usn`:** it is `uuid::<stable>::upnp:rootdevice`. A *stable* uuid —
derived from the library id, not random per process — because a USN that
changes on restart makes every client treat the server as a new device and
re-download the whole media list, which on a library of any size looks
precisely like "DLNA is slow".

### 3.3 The serve loop

```rust
/// Answer searches until the process exits. No shutdown channel — see §3.5
/// for why there is nothing to receive one on.
pub async fn serve_responder(sock: UdpSocket, usn: String, location_base: String) {
    let mut buf = vec![0u8; 2048];
    loop {
        // A receive error is not fatal: a malformed datagram from a scanner on
        // the LAN must not take the responder down. `continue` rather than
        // `break` is the whole difference between a resilient responder and
        // one that dies on the first junk packet a neighbouring device sends.
        let (n, from) = match sock.recv_from(&mut buf).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "dlna: recv failed");
                continue;
            }
        };
        let payload = String::from_utf8_lossy(&buf[..n]);
        if !is_search_for(&payload, MEDIA_SERVER_DEVICE_TYPE) {
            continue;
        }
        // A multicast M-SEARCH is sent to 239.255.255.250:1900, but a
        // unicast retry to the socket's own address is common and must be
        // answered too -- a test client does exactly that, and a responder
        // that only replies to the multicast address is untestable without
        // real multicast.
        let location = format!("{location_base}/dlna/description.xml");
        let reply = search_reply(&usn, &location);
        if let Err(e) = sock.send_to(reply.as_bytes(), from).await {
            tracing::warn!(error = %e, "dlna: reply failed");
        }
    }
}
```

### 3.4 The device description and the browse handler

`GET /dlna/description.xml` — a `DeviceDescription` document. It is small and
static apart from the `LOCATION`, so build it with `format!` from the config;
do not add a template engine for 20 lines of XML.

`ContentBrowse` is a SOAP POST at `/dlna/control`, which is an axum route
taking a `String` body. **The browse of the root object lists only what
`media_path` permits.** That is the whole security model of this ticket and it
belongs in a function you can call from a test:

```rust
/// The DIDL-Lite document for a browse of `object_id`.
///
/// `object_id` of `"0"` is the root, which lists the object *types* this
/// server has and not the objects themselves — a DLNA client navigates
/// root → containers → items, and answering a root browse with a flat file
/// list makes a client show one screen with no way to drill in.
pub async fn content_browse(
    state: &AppState,
    object_id: &str,
) -> Result<String, BrowseError> {
    // THE GATE. Every other route in this server opens with exactly this
    // call, and it is the only one that consults consent. This is the one line
    // in the ticket that matters most, and it lives here rather than in the
    // route so a test can call `content_browse` directly. There is no
    // `get_object` in the store to reach for instead — `media_path` is the
    // only entry point, which is precisely why it is hard to bypass by
    // accident.
    if state.store.media_path(object_id, &local_caller()).await?.is_none() {
        return Err(BrowseError::NotFound);
    }
    // ... build DIDL-Lite
}
```

`BrowseError` is `#[derive(thiserror::Error)]` with `NotFound` and
`Store(#[from] StoreError)`.

### 3.5 Wire the routes

`crates/commons-server/src/lib.rs` gains:

```rust
.route("/dlna/description.xml", get(dlna::get_description))
.route("/dlna/control", post(dlna::content_browse_route))
```

**Both are gated on the same `media_path` check as every other route.** The
description document is not sensitive, but `content_browse_route` is, and a
route that is registered without the gate is the leak the spec's §4.2 is about.

The responder task starts in **`run()` in `src/lib.rs`**, not `main.rs` —
that file only parses the config and calls `run` — and **only when
`config.dlna.enabled`**:

```rust
// After `state` is built and BEFORE `axum::serve`.
let _dlna_task = if state.config.dlna.enabled {
    match dlna::bind_responder(&state.config.dlna.bind).await {
        Ok(sock) => {
            let usn = dlna::stable_usn(&data_dir);
            let base = state.config.dlna.location_base.clone();
            Some(tokio::spawn(dlna::serve_responder(sock, usn, base)))
        }
        Err(e) => {
            // A bind failure is a WARNING, not a startup failure. The HTTP
            // server is fine, and an operator who asked for DLNA on a machine
            // where port 1900 is taken should still get a working library.
            tracing::warn!(error = %e, "dlna: could not bind, discovery is off");
            None
        }
    }
} else {
    None
};
```

`serve_responder` therefore takes **no shutdown channel**, and the
`tokio::select!` on `shutdown.changed()` in §3.3 goes with it. `run()` already
has `with_graceful_shutdown(shutdown_signal())` for the HTTP server;
`shutdown_signal` is a private future, not a channel, and a `watch::Receiver`
invented for this would need plumbing that does not exist in this repository
yet. When `run()` returns, the process is exiting and the task dies with it.

**`if config.dlna.enabled` is the requirement, not a convenience.** Without it
the socket binds on every start and the feature is on in production while
reading as off in the config.

### 3.6 Verify — A1

`crates/commons-server/tests/dlna_discovery.rs`

The test does a **real** `M-SEARCH` over a **real** UDP socket:

```rust
#[tokio::test]
async fn a_search_is_answered_with_a_200_and_a_location() {
    // Port 0: the OS picks a free port, so two tests cannot collide and the
    // suite does not need 1900 to be free on the build machine.
    let sock = dlna::bind_responder("127.0.0.1:0").await.expect("bind");
    let local = sock.local_addr().expect("the chosen port");
    // The task is abandoned, not stopped: `serve_responder` has no shutdown
    // channel, and a test that needs one has found out something the design
    // says is not there. The socket closes when the task drops at the end of
    // the test, and port 0 means it was never contended anyway.
    tokio::spawn(dlna::serve_responder(
        sock,
        "uuid:test::upnp:rootdevice".into(),
        "http://127.0.0.1:9999".into(),
    ));

    let client = UdpSocket::bind("127.0.0.1:0").await.expect("a client socket");
    client
        .send_to(
            b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 1\r\nST: ssdp:all\r\n\r\n",
            local,
        )
        .await
        .expect("the search");

    // A timeout, not an infinite recv: a responder that is wrong must fail
    // this test in a second, not hang it.
    let mut buf = [0u8; 2048];
    let (n, _) = tokio::time::timeout(
        Duration::from_secs(5),
        client.recv_from(&mut buf),
    )
    .await
    .expect("a reply within 5s -- no reply means no responder")
    .expect("recv");

    let reply = String::from_utf8_lossy(&buf[..n]);
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(reply.contains("LOCATION: http://127.0.0.1:9999/dlna/description.xml"), "{reply}");
    assert!(reply.contains("CACHE-CONTROL:"), "some clients re-search without it: {reply}");
}
```

Plus, and these are the ones that make A1 mean something:

- `a_search_for_something_else_is_ignored` — an `M-SEARCH` for
  `urn:schemas-upnp-org:device:MediaRenderer:1` gets **no reply**. Assert with
  `timeout(300ms, recv)` and assert it **timed out**. A responder that answers
  everything claims to be a printer.
- `a_search_is_matched_case_insensitively` — `m-search` lowercase.
- `an_absent_object_and_a_denied_object_browse_identically` — the byte
  comparison, exactly as `an_absent_object_and_a_denied_object_answer_byte_identical_404s`
  does it. **Reuse that test's shape rather than writing a new one.**

### 3.7 Verify

```sh
cargo test -p commons-server --test dlna_discovery
```

**Expected:** `test result: ok.`, **at least 4 passed, 0 failed.** A test that
says "0 passed" means the file did not compile or the filter matched nothing —
read the whole output, not the exit code.

**Commit:** `feat: a DLNA responder that serves only what the gate permits`

---

## Step 4 — the milestone

### 4.1 The final gate, all of it

```sh
export CARGO_TARGET_DIR=/home/alvaro/.cargo-target/commons
export PGHOST=127.0.0.1 PGUSER=postgres PGPASSWORD=smoke_pw
export DATABASE_URL="postgres://postgres:smoke_pw@127.0.0.1/postgres"
cargo build --workspace
cargo test --workspace -- --test-threads=1
cargo clippy --workspace --all-targets 2>&1 | grep -cE '^warning: [a-z]'
```

**Expected:** build clean; **0 failed**; clippy **0**.

**Record the pre-ticket counts BEFORE the first commit** if you intend to
state a delta. `git worktree add /tmp/base <the commit before step 1>` and run
the same command there. A count from a run taken after step 1 is not a
baseline — see the T-P6-004b commit message for a number that was wrong for
exactly that reason.

### 4.2 Docs

- `docs/HANDOFF.md` — a section: what was built, the default-off decision and
  why, and that AirPlay and the bitcode paths are deliberately not here.
- `docs/plans/implementation-plan.md` — T-P6-005's Progress line to **done**.
- `~/secondbrain/90-Meta/2026-09-28-master-build-todo.md` — the stale status
  table needs updating regardless: it still says commons is at `81e4960` and
  lists T-P6-004b as next.

### 4.3 Tag and mirror

```sh
git tag -a phase-7-070-cast-dlna-external-players -m "..."
cd ~/code/rust/commons && git fetch origin --tags && git merge --ff-only origin/main
# then confirm both rev-parse HEAD values are identical
```

---

## What to do if a step's test passes before the feature is written

Stop and check the test actually ran. This repository has been bitten three
times by this and the symptom is always the same: a suite that reports green
in 0.00s, or a filter that matched nothing and reads as "nothing broke".

- `test result: ok. 0 passed` is a FAILURE to investigate, not a pass.
- This repository's own warning: a fixture with a FIXED id passes exactly once
  and then dies on the primary key against a database that persists. Every
  fixture here derives its id from a fresh `Uuid::new_v4()`.
- **Run a new test TWICE.** The second run is against a database that already
  has the first run's rows, which is where a fixed-id fixture and a missing
  unique constraint both show up.
