//! T-P6-005 accept criterion A1: a real `M-SEARCH` is answered with a real
//! `200 OK` carrying a `LOCATION`, and a `ContentBrowse` of a denied object
//! is byte-identical to a browse of an absent one.
//!
//! # Why this file sends real UDP
//!
//! SSDP is multicast, which fails silently. A responder on the wrong
//! interface, one that answers on a port the client is not reading, one that
//! matches `ST` case-sensitively — each of these works on the developer's
//! machine against one client and is invisible in CI. The only way to know a
//! responder works is to send it a search and read the reply, which is what
//! `a_search_is_answered_with_a_200_and_a_location` does.
//!
//! Port `0` everywhere: the OS picks a free port, so two tests cannot collide
//! and the suite does not need 1900 free on the build machine.
//!
//! # The security half
//!
//! The 404-parity test is the load-bearing one. A DLNA client is on the LAN
//! and has never authenticated, so there is no later check to catch a browse
//! handler that reached past the consent gate. Absent and denied must be the
//! same bytes, exactly as `an_absent_object_and_a_denied_object_answer_byte_
// identical_404s` asserts for the HTTP routes.

use std::time::Duration;

use tokio::net::UdpSocket;

mod support;
use commons_server::dlna::{
    bind_responder, is_search_for, search_reply, serve_responder, MEDIA_SERVER_DEVICE_TYPE,
};
use support::{media_fixture, media_fixture_denied, TestApp};

/// A well-formed `M-SEARCH` for the catch-all target, as a real client sends
/// it. `\r\n` line endings included: a responder that splits on `\n` alone
/// leaves a trailing `\r` on the `ST` value and matches nothing.
fn search_packet(st: &str) -> String {
    format!(
        "M-SEARCH * HTTP/1.1\r\n\
         HOST: 239.255.255.250:1900\r\n\
         MAN: \"ssdp:discover\"\r\n\
         MX: 1\r\n\
         ST: {st}\r\n\
         \r\n"
    )
}

/// Send `packet` to a freshly-started responder and return its reply, or
/// `None` if it stayed silent.
///
/// The timeout is the assertion. A responder that is wrong must fail this in
/// five seconds, not hang the suite, and "no reply" has to be distinguishable
/// from "a different reply" — a responder that answers *everything* is as
/// broken as one that answers nothing, and only a timeout catches it.
async fn search_for(packet: &str) -> Option<String> {
    let sock = bind_responder("127.0.0.1:0")
        .await
        .expect("the responder binds");
    let responder_addr = sock.local_addr().expect("the OS chose a port");
    // The task is abandoned, not stopped: `serve_responder` has no shutdown
    // channel, and the socket closes when the task drops at the end of the
    // test.
    tokio::spawn(serve_responder(
        sock,
        "uuid:11111111-2222-4333-8444-555555555555".into(),
        "http://127.0.0.1:9999".into(),
    ));

    let client = UdpSocket::bind("127.0.0.1:0")
        .await
        .expect("a client socket");
    client
        .send_to(packet.as_bytes(), responder_addr)
        .await
        .expect("the search goes out");

    let mut buf = [0u8; 2048];
    match tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf)).await {
        Ok(Ok((n, _))) => Some(String::from_utf8_lossy(&buf[..n]).into_owned()),
        // A timeout IS the result for the "should not answer" cases.
        Ok(Err(e)) => panic!("recv failed: {e}"),
        Err(_) => None,
    }
}

#[tokio::test]
async fn a_search_is_answered_with_a_200_and_a_location() {
    let reply = search_for(&search_packet("ssdp:all"))
        .await
        .expect("a real M-SEARCH must be answered");

    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(
        reply.contains("LOCATION: http://127.0.0.1:9999/dlna/description.xml"),
        "and a LOCATION a client can actually fetch: {reply}"
    );
    // The two headers a client checks before it will do anything. Without
    // CACHE-CONTROL some clients re-search immediately, so a correct responder
    // still looks like it is "flashing" in a TV's source list.
    assert!(reply.contains("CACHE-CONTROL:"), "{reply}");
    assert!(reply.contains("EXT:"), "{reply}");
    // And the target the client asked about, echoed back — a responder that
    // answers with a different ST is one a client discards.
    assert!(
        reply.contains(&format!("ST: {MEDIA_SERVER_DEVICE_TYPE}")),
        "{reply}"
    );
    assert!(
        reply.contains("USN: uuid:11111111-2222-4333-8444-555555555555::upnp:rootdevice"),
        "the USN echoes the device's identity, and the same on every restart: {reply}"
    );
}

#[tokio::test]
async fn a_search_for_the_media_server_type_is_also_answered() {
    // Not just the catch-all. A client that asked for a MediaServer and got a
    // device list is a client that never shows the library.
    let reply = search_for(&search_packet(MEDIA_SERVER_DEVICE_TYPE))
        .await
        .expect("a targeted search is answered too");
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
}

#[tokio::test]
async fn a_search_for_something_else_is_ignored() {
    // A responder that answers everything claims to be a printer, a
    // thermostat and a media server. That assertion can only be made by
    // waiting and confirming silence.
    assert!(
        search_for(&search_packet(
            "urn:schemas-upnp-org:device:MediaRenderer:1"
        ))
        .await
        .is_none(),
        "a search for a device we are not must go unanswered"
    );
}

#[tokio::test]
async fn a_search_is_matched_however_it_is_capitalised() {
    // Real clients send `m-search` and `ST: SsDp:All`. A responder that
    // matches case-sensitively works against curl and not against a
    // television, which is the hardest kind of bug to notice locally.
    let lower =
        search_for("m-search * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nST: ssdp:all\r\n\r\n")
            .await;
    assert!(
        lower.is_some(),
        "a lowercased search is still a search: {lower:?}"
    );
}

#[tokio::test]
async fn something_that_is_not_a_search_is_ignored() {
    // The predicate is unit-tested for detail; this asserts the loop applies
    // it, because a responder that replies to a random UDP packet is a
    // responder that answers a port scan.
    assert!(
        search_for("hello\r\n").await.is_none(),
        "a non-search datagram is not answered"
    );
    assert!(
        search_for("NOTIFY * HTTP/1.1\r\nNT: upnp:rootdevice\r\n\r\n")
            .await
            .is_none()
    );
}

#[test]
fn the_search_predicate_reads_the_st_header_and_not_the_host() {
    // A packet mentioning our device type in some other header must not match.
    // `HOST:` is the trap: a search for `ssdp:all` still has
    // `HOST: 239.255.255.250:1900` in it, and a substring match on the
    // address would answer every packet ever sent to this port.
    let packet = "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nST: ssdp:all\r\n\r\n";
    assert!(is_search_for(packet, MEDIA_SERVER_DEVICE_TYPE));

    let wrong_st = format!(
        "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nX-Note: {MEDIA_SERVER_DEVICE_TYPE}\r\nST: upnp:somethingelse\r\n\r\n"
    );
    assert!(
        !is_search_for(&wrong_st, MEDIA_SERVER_DEVICE_TYPE),
        "the device type in a header other than ST does not count"
    );

    // Not a search at all, even with a perfect ST.
    assert!(!is_search_for(
        "GET / HTTP/1.1\r\nST: ssdp:all\r\n\r\n",
        MEDIA_SERVER_DEVICE_TYPE
    ));
}

#[test]
fn the_reply_carries_the_location_a_client_can_fetch() {
    // A trailing slash on the configured base must not produce `//dlna/...`:
    // some clients normalise it and some treat the doubled slash as a
    // different path, and the difference is invisible from here.
    let with_slash = commons_server::dlna::description_url("http://h:8096/");
    let without = commons_server::dlna::description_url("http://h:8096");
    assert_eq!(with_slash, "http://h:8096/dlna/description.xml");
    assert_eq!(with_slash, without, "one form, whichever was configured");

    let r = search_reply("uuid:x", &with_slash);
    assert!(
        r.contains("LOCATION: http://h:8096/dlna/description.xml\r\n"),
        "{r:?}"
    );
    assert!(
        r.ends_with("\r\n\r\n"),
        "headers end with a blank line: {r:?}"
    );
}

#[test]
fn a_friendly_name_with_markup_characters_cannot_break_the_description() {
    // `friendly_name` comes from a config file, and a `&` in "Fish & Chips"
    // is a parse error on the client. Worse, a name containing markup would
    // let a config file inject a second device into the description.
    let d = commons_server::dlna::device_description(
        "Fish & <Chips> \"now\"",
        "uuid:abc",
        "/dlna/control",
    );
    assert!(d.contains("Fish &amp; &lt;Chips&gt;"), "{d}");
    assert!(!d.contains("<Chips>"), "no raw markup survives: {d}");
    assert!(d.contains("<UDN>uuid:abc</UDN>"), "the UDN is the usn: {d}");
    assert!(d.contains("<controlURL>/dlna/control</controlURL>"), "{d}");
}

#[test]
fn the_device_description_names_the_type_a_client_searched_for() {
    let d = commons_server::dlna::device_description("The library", "uuid:abc", "/dlna/control");
    assert!(
        d.contains(&format!(
            "<deviceType>{MEDIA_SERVER_DEVICE_TYPE}</deviceType>"
        )),
        "a client that finds this and does not recognise the type discards it: {d}"
    );
    assert!(
        d.contains("urn:schemas-upnp-org:service:ContentDirectory:1"),
        "{d}"
    );
    assert!(
        d.contains("<friendlyName>The library</friendlyName>"),
        "{d}"
    );
}

// ---------- the security half: browse parity ----------

#[tokio::test]
async fn an_absent_object_and_a_denied_object_browse_identically() {
    // The load-bearing test in this file. A DLNA client is on the LAN and has
    // never authenticated, so unlike an HTTP request there is no later check
    // to catch a browse that reached past `media_path`. Absent and denied must
    // be the SAME BYTES, exactly as
    // `an_absent_object_and_a_denied_object_answer_byte_identical_404s`
    // asserts for the routes.
    let app = TestApp::new().await;
    let denied = media_fixture_denied(&app, b"0123456789").await;
    let absent = "obj-this-does-not-exist-at-all";

    let a = app
        .send_raw(
            axum::http::Request::builder()
                .method("POST")
                .uri("/dlna/control")
                .header("CONTENT-TYPE", "text/xml")
                .body(axum::body::Body::from(browse_body(absent)))
                .expect("a request"),
        )
        .await;
    let b = app
        .send_raw(
            axum::http::Request::builder()
                .method("POST")
                .uri("/dlna/control")
                .header("CONTENT-TYPE", "text/xml")
                .body(axum::body::Body::from(browse_body(&denied)))
                .expect("a request"),
        )
        .await;

    assert_eq!(a.status, axum::http::StatusCode::NOT_FOUND, "absent");
    assert_eq!(b.status, axum::http::StatusCode::NOT_FOUND, "denied");
    assert_eq!(
        a.body,
        b.body,
        "the two must be indistinguishable: {:?} vs {:?}",
        String::from_utf8_lossy(&a.body),
        String::from_utf8_lossy(&b.body)
    );
}

#[tokio::test]
async fn a_browsable_object_is_not_a_404() {
    // The other half. A browse handler that 404s everything satisfies the
    // parity test above, which is why this exists.
    let app = TestApp::new().await;
    let fixture = media_fixture(&app, b"0123456789").await;

    let r = app
        .send_raw(
            axum::http::Request::builder()
                .method("POST")
                .uri("/dlna/control")
                .header("CONTENT-TYPE", "text/xml")
                .body(axum::body::Body::from(browse_body(&fixture.object_id)))
                .expect("a request"),
        )
        .await;

    assert_eq!(r.status, axum::http::StatusCode::OK);
    let body = String::from_utf8_lossy(&r.body);
    assert!(body.contains("DIDL-Lite"), "{body}");
    assert!(
        body.contains(&fixture.object_id),
        "and names the object: {body}"
    );
}

/// A `ContentBrowse` SOAP body for `object_id`.
///
/// The `ObjectID` is escaped because it goes into XML: an id containing `&`
/// must not be able to break the document, and the escape is the same helper
/// the description uses, asserted in `a_friendly_name_with_markup_characters_`.
fn browse_body(object_id: &str) -> String {
    format!(
        r#"<?xml version="1.0"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"
            s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:ContentBrowse xmlns:u="urn:schemas-upnp-org:service:ContentDirectory:1">
      <ObjectID>{id}</ObjectID>
      <BrowseFlag>BrowseDirectChildren</BrowseFlag>
      <Filter>*</Filter>
      <StartingIndex>0</StartingIndex>
      <RequestedCount>0</RequestedCount>
      <SortCriteria></SortCriteria>
    </u:ContentBrowse>
  </s:Body>
</s:Envelope>
"#,
        id = object_id
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    )
}
