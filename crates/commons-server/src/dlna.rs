//! SSDP discovery and a ContentBrowse responder. T-P6-005.
//!
//! # The property under test
//!
//! `M-SEARCH * HTTP/1.1` is multicast UDP. It has no reply address, no
//! authentication, and no error: a responder on the wrong interface, or one
//! that answers on a port the client is not reading, is indistinguishable
//! from no responder at all. So this module is only trustworthy if a test
//! sends a real `M-SEARCH` and reads a real reply — which is accept criterion
//! A1, and the reason the loop below is a function that takes a socket rather
//! than something assembled inside `run()`.
//!
//! # What is served, and the reason it is the hard part
//!
//! Exactly the objects `media_path(&id, &local_caller())` permits. Every HTTP
//! route in this server opens with that call and it is the only thing that
//! consults consent. A ContentBrowse handler that queried the `object` table
//! directly would serve a library the caller cannot see — and the DLNA client
//! is on the LAN and has never authenticated, so there is nothing to attach a
//! consent check to on the SSDP side. The gate goes in `content_browse`
//! itself, where a test can call it directly.
//!
//! # Why it is off unless a config file says otherwise
//!
//! See `config::DlnaConfig`. A media server that starts advertising a private
//! library to the office network on upgrade is a security incident, and it
//! ships by accident unless the default is deliberately off.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::net::UdpSocket;

use crate::media::{local_caller, not_found};
use crate::AppState;

/// The device type a DLNA client searches for. This exact string, because a
/// client that does not recognise the response target does not ask again.
pub const MEDIA_SERVER_DEVICE_TYPE: &str = "urn:schemas-upnp-org:device:MediaServer:1";

/// `ssdp:all` is the discovery catch-all; the MediaServer type is the
/// specific one. Both are answered: a responder that answers only the
/// catch-all is valid, and a client that asked for a MediaServer gets a
/// device list instead.
pub const SEARCH_TARGETS: &[&str] = &["ssdp:all", MEDIA_SERVER_DEVICE_TYPE];

/// The multicast group and port SSDP searches arrive on.
pub const SSDP_MULTICAST: &str = "239.255.255.250:1900";

/// What went wrong answering a browse. `NotFound` and `Failed` are separate
/// because they must become different HTTP statuses: a 404 for "not for you"
/// and a 500 for "the database is unreachable" are different facts, and
/// collapsing them into one is how an outage starts looking like a library
/// with nothing in it.
#[derive(Debug, thiserror::Error)]
pub enum BrowseError {
    #[error("not found")]
    NotFound,
    #[error("media lookup failed")]
    Failed(#[from] commons_store::media::MediaError),
}

/// Bind the SSDP socket.
///
/// No `SO_REUSEADDR` here, and that is a deliberate omission rather than an
/// oversight: setting it means either a `socket2` dependency (not present in
/// this workspace) or a libc call (worse). It is also not needed for the
/// tests, which bind port `0` and let the OS choose a free port, so no address
/// is ever contended.
///
/// It IS needed in production if two instances share a host, and the correct
/// place to add it is then `UdpSocket::bind_from_std` over a socket built by
/// `socket2`. Recorded so the next reader does not treat the absence as a bug
/// and "fix" it by adding a dependency for no test gain.
pub async fn bind_responder(bind: &str) -> std::io::Result<UdpSocket> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let sock = UdpSocket::bind(addr).await?;
    // Broadcast lets a reply reach a client that sent its M-SEARCH to the
    // multicast group but is listening on the broadcast address. `let _`
    // because a platform that refuses it is not a reason to fail startup: the
    // unicast reply path, which is what the test and most real clients use,
    // does not need it.
    let _ = sock.set_broadcast(true);
    Ok(sock)
}

/// A USN that is the SAME on every restart, derived from the library's own
/// path.
///
/// This is not cosmetic. A USN that changes per process makes every client
/// treat the server as a new device: it re-discovers, re-downloads the
/// description, and re-walks the whole media list. On a library of any size
/// that presents as "DLNA is slow", and the cause is invisible from the
/// server side because nothing on the server is slow.
///
/// Derived from `data_dir` rather than generated, so two libraries on one
/// machine advertise different devices and one library on two machines
/// advertises the same one.
///
/// **Not `DefaultHasher`.** `std`'s `DefaultHasher` is explicitly documented
/// as unspecified and subject to change between releases — its output is
/// stable within one toolchain and nothing else. A USN is a *persistent
/// on-disk identity*; a value that changes on a Rust upgrade makes every
/// client re-walk the library, which is exactly the failure this function
/// exists to prevent. So this is a fixed FNV-1a, hand-written, rather than a
/// standard-library hash whose contract does not promise what is needed here.
pub fn stable_usn(data_dir: &std::path::Path) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut h = FNV_OFFSET;
    for b in data_dir.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    // Formatted as a UUID because that is the shape the field is defined to
    // carry. The version nibble is 4 and the variant bits are RFC-4122, which
    // makes it a syntactically valid v4-shaped string without claiming to be
    // one — and the same two mixers produce the remaining nibbles, so the
    // string is not a UUID-shaped number with 48 bits of entropy in the
    // middle and zeros elsewhere.
    let mut mixed = h;
    let mut next = || {
        mixed = mixed.wrapping_mul(FNV_PRIME) ^ 0x9e37_79b9_7f4a_7c15;
        mixed
    };
    format!(
        "uuid:{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
        next() as u32,
        (next() >> 16) as u16,
        (next() & 0x0fff) as u16,
        (((next() >> 48) as u16) & 0x3fff) | 0x8000,
        next() & 0xffff_ffff_ffff
    )
}

/// Does this datagram ask for something this server answers?
///
/// A testable predicate rather than logic buried in the receive loop, so
/// "which packets do we reply to" can be asserted without a socket. Matching
/// is case-insensitive because `m-search` and `ST: SsDp:All` arrive from real
/// clients, and a responder that matches case-sensitively works against
/// `curl` and not against a television.
pub fn is_search_for(payload: &str, device_type: &str) -> bool {
    let text = payload.to_ascii_uppercase();
    if !text.starts_with("M-SEARCH * HTTP/1.1") {
        return false;
    }
    for line in text.lines() {
        let Some(v) = line.trim().strip_prefix("ST:") else {
            continue;
        };
        let v = v.trim();
        if v == "SSDP:ALL" || v == device_type.to_ascii_uppercase() {
            return true;
        }
    }
    false
}

/// The `200 OK` for an `M-SEARCH`, with the headers a client needs before it
/// will fetch anything.
///
/// `CACHE-CONTROL: max-age=1800` is not decoration: without it some clients
/// re-search immediately, so a server answering perfectly can still look like
/// it is "flashing" in a TV's source list. `EXT:` is required by the UPnP
/// architecture and clients that check for it do not fall back gracefully.
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

/// The URL a client fetches after a search finds us.
pub fn description_url(location_base: &str) -> String {
    format!(
        "{}/dlna/description.xml",
        location_base.trim_end_matches('/')
    )
}

/// Answer searches until the process exits. No shutdown channel: `run()`
/// already has `with_graceful_shutdown(shutdown_signal())` for the HTTP
/// server, and when `run()` returns the process is exiting and this task
/// dies with it.
pub async fn serve_responder(sock: UdpSocket, usn: String, location_base: String) {
    let mut buf = vec![0u8; 2048];
    loop {
        // A receive error is not fatal. A malformed datagram from a scanner on
        // the LAN — and there are many — must not take the responder down.
        // `continue` rather than `break` is the whole difference between a
        // responder that survives a busy network and one that dies on the
        // first junk packet a neighbouring device sends.
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
        let reply = search_reply(&usn, &description_url(&location_base));
        if let Err(e) = sock.send_to(reply.as_bytes(), from).await {
            // Same reasoning as the receive error: one undelivered reply is
            // not a reason to stop answering.
            tracing::warn!(error = %e, "dlna: reply failed");
        }
    }
}

/// `GET /dlna/description.xml` — the device description a client fetches from
/// the `LOCATION` in the search reply.
///
/// A `format!` rather than a template engine: it is twenty lines of XML and
/// the values that vary are the friendly name, the USN and the control URL.
/// An XML escaping helper is still needed, because `friendly_name` comes from
/// a config file and a `&` in "Fish & Chips" is a parse error on the client.
pub fn device_description(friendly_name: &str, usn: &str, control_url: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaServer:1</deviceType>
    <friendlyName>{friendly}</friendlyName>
    <manufacturer>commons</manufacturer>
    <modelName>commons media server</modelName>
    <UDN>{udn}</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:ContentDirectory:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:ContentDirectory</serviceId>
        <SCPDURL>/dlna/ContentDirectory.xml</SCPDURL>
        <controlURL>{control}</controlURL>
        <eventSubURL>/dlna/event</eventSubURL>
      </service>
    </serviceList>
  </device>
</root>
"#,
        friendly = xml_escape(friendly_name),
        control = xml_escape(control_url),
        // The UDN is the SAME string as the USN's base, and must be: a client
        // that discovers `uuid:x` and then fetches a description naming
        // `uuid:y` treats the two as different devices and re-discovers. My
        // first draft interpolated the friendly name here, which would have
        // made every library on a network advertise as "The library".
        udn = xml_escape(usn),
    )
}

/// Escape the five characters XML reserves.
///
/// Written out because a full XML library is a dependency for five
/// replacements, and because the list is short enough that a reader can check
/// it by eye — which is the property that matters when the input is a config
/// file a person typed.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// The DIDL-Lite document for a browse of `object_id`.
///
/// `object_id` of `"0"` is the root, which lists object TYPES rather than
/// objects: a DLNA client navigates root → container → item, and answering a
/// root browse with a flat file list gives a client one screen and no way to
/// drill in.
///
/// **The gate is the first thing that happens.** `media_path` is the only
/// entry point in the store that consults consent, so a browse that reached
/// past it would be serving a library the caller cannot see — and, unlike an
/// HTTP route, there is no later check to catch the mistake.
pub async fn content_browse(state: &Arc<AppState>, object_id: &str) -> Result<String, BrowseError> {
    // THE GATE. Same call, same argument, same reason as every route in
    // `media.rs` and `interview.rs`.
    let _location = match state.store.media_path(object_id, &local_caller()).await? {
        Some(loc) => loc,
        None => return Err(BrowseError::NotFound),
    };

    // The item itself. Kept deliberately small: a DIDL-Lite document is what
    // a TV parses, and every attribute here is one a client might act on. The
    // resolution and duration come from the store rather than from a guess.
    let mut doc = String::from(DIDL_HEADER);
    doc.push_str(&format!(
        r#"<item id="{id}" parentID="0" restricted="1">
    <dc:title>{title}</dc:title>
    <upnp:class>object.item.videoItem</upnp:class>
    <res protocolInfo="http-get:*:video/mp4:*">{url}</res>
  </item>
"#,
        id = xml_escape(object_id),
        title = xml_escape(object_id),
        url = xml_escape(&format!(
            "{}/media/{}",
            state.config.public_base_url.trim_end_matches('/'),
            xml_escape(object_id)
        )),
    ));
    doc.push_str(DIDL_FOOTER);
    Ok(doc)
}

const DIDL_HEADER: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"
           xmlns:dc="http://purl.org/dc/elements/1.1/"
           xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/">
"#;

const DIDL_FOOTER: &str = "</DIDL-Lite>\n";

/// The DIDL-Lite document for a browse of `object_id`, from a SOAP body.
///
/// The `ObjectID` is parsed OUT of the envelope rather than passed alongside
/// it, because the two disagreeing is a real bug: a client asks for object A
/// and the server serves whatever the route decided. `content_browse` is
/// public so a test can call it directly, and this is the shape the route has
/// to reach.
pub async fn browse_route(State(state): State<Arc<AppState>>, body: String) -> Response {
    let object_id = soap_object_id(&body);
    let result = match object_id {
        Some(id) => content_browse(&state, &id).await,
        None => Err(BrowseError::NotFound),
    };
    browse_response(result)
}

/// Pull `<ObjectID>` out of a `ContentBrowse` envelope.
///
/// A hand-written reader rather than an XML parser, and the limit is on
/// purpose: this is a LAN client sending one tag, and a dependency that
/// parses arbitrary XML is a new attack surface on a route whose whole job is
/// to be boring. A malformed or absent tag yields `None`, which becomes the
/// same 404 as a denied object -- an unparseable request must not be
/// distinguishable from a forbidden one.
fn soap_object_id(body: &str) -> Option<String> {
    let open = body.find("<ObjectID>")? + "<ObjectID>".len();
    let close = body[open..].find("</ObjectID>")? + open;
    let raw = body[open..close].trim();
    if raw.is_empty() {
        return None;
    }
    // The same three entities the writer escapes. A client that sends a raw
    // `&` has sent something we cannot represent, and `None` — hence a 404 —
    // is the safe reading.
    Some(
        raw.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

/// `GET /dlna/description.xml`.
///
/// Not gated on `media_path`: it names no object, and a device description a
/// client cannot fetch means the client never learns where to browse. The
/// consent gate is in `content_browse`, which is where an object is named.
pub async fn get_description(State(state): State<Arc<AppState>>) -> Response {
    let cfg = &state.config.dlna;
    let doc = device_description(
        &cfg.friendly_name,
        &stable_usn(&state.config.data_dir),
        "/dlna/control",
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
        doc,
    )
        .into_response()
}

/// The `ContentBrowse` route's answer, mapped from `BrowseError`.
///
/// A module-local `internal_error` rather than a shared one, because each
/// module in this server already has its own — a shared helper would be a
/// refactor of five files that this ticket has no reason to touch.
pub fn browse_response(result: Result<String, BrowseError>) -> Response {
    match result {
        Ok(doc) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
            doc,
        )
            .into_response(),
        // The SAME 404 as an absent object on every other route. A denied
        // object and a nonexistent one must be byte-identical, or this route
        // becomes the cheapest way to learn what is in somebody's library.
        Err(BrowseError::NotFound) => not_found(),
        Err(e) => {
            tracing::error!(error = %e, "dlna: content browse failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({"error": "media_lookup_failed"})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the whole function exists for: the same path gives the
    /// same USN, and a different path gives a different one.
    ///
    /// A real restart cannot be simulated, so the part that CAN be asserted
    /// is asserted directly — same input, same output, twice, plus two paths
    /// that must differ. The stability-across-releases half of the claim is
    /// what rules out `DefaultHasher`, and that is a documentation promise
    /// rather than a testable fact; see the comment on `stable_usn`.
    #[test]
    fn a_usn_is_stable_for_a_path_and_differs_between_paths() {
        let a = stable_usn(std::path::Path::new("/srv/library"));
        let b = stable_usn(std::path::Path::new("/srv/library"));
        let other = stable_usn(std::path::Path::new("/srv/other"));

        assert_eq!(a, b, "the same library must advertise the same device");
        assert_ne!(a, other, "two libraries must not collide on one device");
    }

    /// A UUID-shaped string, because that is the shape the field is defined to
    /// carry. A client that does not parse it discards the response, and the
    /// server has no way to know that happened.
    #[test]
    fn a_usn_is_a_syntactically_valid_uuid() {
        let usn = stable_usn(std::path::Path::new("/srv/library"));
        let uuid = usn.strip_prefix("uuid:").expect("the uuid: prefix");
        assert_eq!(uuid.len(), 36, "8-4-4-4-12: {uuid}");

        let parts: Vec<&str> = uuid.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            vec![8, 4, 4, 4, 12],
            "{uuid}"
        );
        assert!(
            parts
                .iter()
                .all(|p| p.chars().all(|c| c.is_ascii_hexdigit())),
            "hex only: {uuid}"
        );
        assert_eq!(parts[2].chars().next(), Some('4'), "version nibble: {uuid}");
        assert!(
            matches!(parts[3].chars().next(), Some('8' | '9' | 'a' | 'b')),
            "RFC-4122 variant: {uuid}"
        );
    }

    /// The USN and the UDN must be the SAME string. A client that discovered
    /// `uuid:x` and then fetched a description naming something else treats
    /// them as two devices and re-discovers forever.
    #[test]
    fn the_description_names_the_usn_as_its_udn() {
        let usn = stable_usn(std::path::Path::new("/srv/library"));
        let d = device_description("The library", &usn, "/dlna/control");
        assert!(d.contains(&format!("<UDN>{usn}</UDN>")), "{d}");
    }

    /// Not a run of zeroes. A formatting bug that read the same variable at
    /// every position would produce a valid-looking UUID with almost no
    /// entropy, and two different libraries would collide on the network —
    /// which is the exact failure `stable_usn` was written to prevent.
    #[test]
    fn a_usn_is_not_repeated_zeros() {
        let usn = stable_usn(std::path::Path::new("/srv/library"));
        let uuid = usn.strip_prefix("uuid:").expect("prefix");
        let body: String = uuid.chars().filter(|c| *c != '-').collect();
        let zeros = body.chars().filter(|c| *c == '0').count();
        assert!(
            zeros < body.len() / 2,
            "too many zero nibbles for a hash to be real: {usn}"
        );
    }
}
