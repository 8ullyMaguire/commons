---
title: T-P6-005 — Cast, DLNA, external players
date: 2026-09-28
status: proposed
ticket: T-P6-005
platform-spec: §11.2, §11.3
---

# T-P6-005 — Cast, DLNA, external players

**Status: not started.** Written after T-P6-004b was completed and verified
(`phase-7-060-interview-derivation`), which was the last ticket ahead of this
one in the plan's Phase 6 order.

**Platform spec:** `~/secondbrain/10-Projects/2026-09-26T110000+0200-commons-platform-spec.md`
**§11.2** (cast and network playback) and **§11.3** (external players). Read both
before implementing; §4.4 is load-bearing for neither but governs the sandbox the
plugin SDK later needs.

**Parent commit:** `8976fa5` (`phase-7-060-interview-derivation`).

---

## 1. What exists, and what the gap actually is

Checked before writing, because the last three tickets each turned out to be
mostly already built:

| Thing | State |
|---|---|
| `commons-server/src/dlna.rs` | **absent** |
| `commons-server/src/cast.rs` | **absent** |
| `commons-server/src/external_player.rs` | **absent** |
| SSDP / UPnP anywhere in the workspace | **absent** (`grep -rl 'ssdp\|upnp\|239\.255'` → nothing) |
| An argv built for an external player | **absent** (no `Command::new` in `commons-server/src`) |
| A resume position | **exists** — `commons-store/src/playback.rs`, `GET`/`PUT /media/:id/playback` |
| An m3u8 proxy | **exists** — `commons-server/src/proxy.rs`, 445 lines, byte-range aware |

So this is a genuinely new ticket. Two of its three parts turn out to be
*assembly* rather than invention, and the plan says so explicitly so the
implementer does not rebuild them:

- **Resume position is a read**, not a feature. `playback::get_playback` already
  returns `position_ms`. An external-player command line that starts at the
  right second is that value passed through.
- **DLNA serving subtitles is a content-directory concern**, not a transcoding
  one. The subtitle bytes already exist (`commons-media/src/subtitles.rs`).

## 2. The three parts, and why they are one ticket

They are one ticket because they share the one thing that is easy to get
wrong and impossible to notice:

**A network-exposed surface that can be probed is not a feature, it is a leak.**
The player routes, the proxy and every route added in T-P6-004/T-P6-004b are
gated on `media_path(&object_id, &local_caller())`, so absent, off-disk and
denied are one 404. A DLNA server advertises itself to the *whole network* — a
client on the LAN can enumerate the entire library without ever authenticating,
because SSDP has no auth mechanism to gate on. That is not a bug in the
implementation; it is the protocol.

So the spec's requirement is not "add DLNA". It is: **when DLNA is on, it serves
exactly the objects a logged-in caller can see, and it is off unless someone
turned it on.** A DLNA server that is on by default and serves the whole library
is a worse outcome than no DLNA server at all, and it is the failure a reader
should worry about when they see a media server that speaks UPnP.

## 3. Acceptance criteria

Two, because the plan names two, and both are the kind that can be verified by
running rather than by reading.

### A1 — A DLNA discovery test: the server advertises and serves a browse

SSDP multicast on loopback: the server responds to an `M-SEARCH` for
`urn:schemas-upnp-org:device:MediaServer:1` with a `200 OK` carrying a
`LOCATION` header, and a `ContentBrowse` request against that location returns
a DIDL-Lite document listing the objects the caller may see.

**Why a test and not a manual check:** SSDP is multicast UDP, which fails
silently. A responder that binds the wrong interface, or answers on the
interface the client did not ask from, works on the developer's machine and is
invisible in CI. The only way to know is to send an `M-SEARCH` and read the
reply, which is what the test does.

### A2 — The external-player command line is an argv array, compared exactly

`external_player_command(player, object, …)` returns `Vec<OsString>` (or a
`Vec<String>` of already-escaped argv). The test asserts the returned array
**equals** an expected array — not that a rendered string contains a substring.

**Why argv and never a shell string:** a title containing `; rm -rf ~` is data.
The moment it is concatenated into `sh -c "mpv …"`, it is code. This is not
hypothetical — media titles come from scraped sources and from filenames, both
of which contain shell metacharacters in practice. The test must include a title
with a metacharacter in it, or it is not testing the property it claims to.

## 4. The five ways this is wrong

In the house style of the parent specs, because each of these is a real way the
naive implementation fails and none of them errors.

1. **The DLNA server is on by default.** The spec says nothing about defaults,
   so the default must be off, and `config.toml` must be the only thing that
   turns it on. A media server that starts advertising a private library to the
   office network on upgrade is a security incident, and it will be shipped by
   accident unless the default is deliberately off.
2. **The 404 gate is forgotten on the DLNA path.** The HTTP routes all call
   `media_path(&object_id, &local_caller())`. A ContentBrowse handler that
   queries `object` directly serves a library the caller cannot see. The gate
   must be in the *browse* handler, and a test must assert an absent object and
   a denied object produce the same response — the same oracle the HTTP routes
   use.
3. **`127.0.0.1` as a bind address and `0.0.0.0` as an answer.** A responder
   that binds loopback but advertises `0.0.0.0` in its `LOCATION` hands a LAN
   client an address that means "this machine" and fails in a way that looks
   like the TV is broken. And a responder that binds `0.0.0.0` while the config
   says loopback is the leak from (1) in a different disguise.
4. **Shell interpolation of a title.** See A2. Also: an `mpv` argv that quotes
   correctly on a POSIX shell is still wrong on Windows, so the builder is
   argv-only with no quoting logic at all — quoting is `Command`'s job.
5. **Resume position ignored or fudged.** `position_ms` is the honest value;
   rounding it to the nearest 10 s "to be kind" is a different claim than the
   one the store holds, and a user who closed a video at 1:23:47 and reopens it
   at 1:23:40 has been lied to in a way that is impossible to debug later.

## 5. Not in this ticket

- **Bitcode / hardware paths for cast targets** (spec §11.2). Those are a
  performance feature that needs a real device to measure against; there is no
  device in CI and a fake one would be a test of a mock.
- **AirPlay** (spec §11.2). A separate protocol from DLNA with its own pairing
  and discovery model. Folding it in would make A1 ambiguous — "a discovery
  test" for two protocols is two tests wearing one name.
- **The plugin SDK** (T-P6-006) and the **public API** (T-P6-007). Both are
  untouched by this ticket and neither waits on it.

## 6. Files

New:
- `crates/commons-server/src/dlna.rs` — SSDP responder + ContentBrowse.
- `crates/commons-server/src/external_player.rs` — argv builder, no I/O.
- `crates/commons-server/tests/dlna_discovery.rs` — A1.
- `crates/commons-server/tests/external_player_argv.rs` — A2.

Modified:
- `crates/commons-server/src/lib.rs` — module declarations and routes.
- `crates/commons-server/src/config.rs` — the `dlna` config block, default off.
- `crates/commons-server/Cargo.toml` — nothing new is needed; `tokio` already
  carries `full`, so `UdpSocket` is available. Recorded so the implementer does
  not go looking for a missing feature.
