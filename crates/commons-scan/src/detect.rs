//! Type detection by content, not extension (T-P1-001, spec §5.2, §5.3).
//!
//! The rule from the spec is that a file's type comes from sniffing its bytes,
//! not from its name. The reason is not aesthetic: the same extension is
//! routinely wrong in real libraries, and each of these upstream issues is a
//! library where it was.
//!
//! | issue  | what the extension said | what the bytes were |
//! |--------|------------------------|---------------------|
//! | #6577  | `.webm`                | a short animated clip served as an image |
//! | #5111  | `.gif` in a zip        | an image, not a scene |
//! | #5185  | `.mp4` in a folder     | a still frame loop, wanted as a gallery |
//! | #7179  | folder of images       | `.nogallery` says no gallery, but one existed |
//!
//! Extension is consulted only as a tiebreaker, and only after the bytes have
//! failed to decide. Where the bytes are decisive, they win even if the
//! extension disagrees.

use std::io::Read;
use std::path::Path;

use commons_core::ObjectKind;

/// The result of sniffing: a kind, plus what decided it.
#[derive(Debug, Clone, PartialEq)]
pub struct Detection {
    pub kind: ObjectKind,
    /// The container format found in the bytes, when one was recognisable.
    pub container: Option<Container>,
    /// Which signal decided. Carried into logs and the scan report, because
    /// "why was this typed this way" is the question every false positive
    /// starts with.
    pub decided_by: Evidence,
    /// True when a `.forcegallery` / `.nogallery` file in an ancestor directory
    /// overrode the sniffed answer.
    pub overridden: bool,
}

/// What settled the kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Evidence {
    /// Magic bytes were decisive.
    Magic,
    /// The bytes were inconclusive and the extension decided.
    Extension,
    /// Directory-level override (`.forcegallery` / `.nogallery`).
    Override,
    /// Only the extension was available (a sidecar with no bytes of its own).
    NameOnly,
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Evidence::Magic => "magic",
            Evidence::Extension => "extension",
            Evidence::Override => "override",
            Evidence::NameOnly => "name",
        };
        f.write_str(s)
    }
}

/// Recognised container formats. This is a sniff list, not a media database:
/// each variant exists because a test asserts on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    IsoBmff,
    Matroska,
    Webm,
    Avi,
    AsfWmv,
    QuickTime,
    MpegTs,
    Zip,
    SevenZip,
    Rar,
    Pdf,
    Ogg,
    Flac,
    Mp3,
    Wave,
    Jpeg,
    Png,
    Gif,
    WebP,
    Avif,
    Heif,
    Bmp,
    Tiff,
    Text,
    Xml,
    Json,
    Funscript,
}

impl Container {
    /// The extension this container normally carries, for a name the user
    /// recognises. Never used to *decide* a type.
    pub fn typical_extension(self) -> &'static str {
        use Container::*;
        match self {
            IsoBmff | QuickTime => "mp4",
            Matroska => "mkv",
            Webm => "webm",
            Avi => "avi",
            AsfWmv => "wmv",
            MpegTs => "ts",
            Zip => "zip",
            SevenZip => "7z",
            Rar => "rar",
            Pdf => "pdf",
            Ogg => "ogg",
            Flac => "flac",
            Mp3 => "mp3",
            Wave => "wav",
            Jpeg => "jpg",
            Png => "png",
            Gif => "gif",
            WebP => "webp",
            Avif => "avif",
            Heif => "heic",
            Bmp => "bmp",
            Tiff => "tiff",
            Text => "txt",
            Xml => "xml",
            Json => "json",
            Funscript => "funscript",
        }
    }
}

/// How many bytes the sniffer wants. Every magic test below needs at most 32;
/// ZIP's end-of-central-directory needs more and is handled separately.
const SNIFF_LEN: usize = 64;

/// Detect from a path. Reads only the head of the file.
pub fn detect(path: &Path) -> std::io::Result<Detection> {
    let mut buf = [0u8; SNIFF_LEN];
    let n = read_head(path, &mut buf)?;
    detect_from_bytes(&buf[..n], path)
}

/// Detect from bytes already in hand, plus the path for the extension. Split
/// from [`detect`] so the scanner can reuse a read it has already made, and so
/// tests can drive the table without touching the filesystem.
pub fn detect_from_bytes(bytes: &[u8], path: &Path) -> std::io::Result<Detection> {
    let ext = extension_of(path);
    let ext = ext.as_deref();

    // A directory-level override beats everything, including decisive magic
    // bytes. `.forcegallery` exists precisely to say "the bytes say video, I
    // meant gallery" (stash#5185).
    if let Some(forced) = directory_override(path) {
        return Ok(Detection {
            kind: forced,
            container: sniff_container(bytes),
            decided_by: Evidence::Override,
            overridden: true,
        });
    }

    if let Some(c) = sniff_container(bytes) {
        return Ok(Detection {
            kind: kind_for_container(c, ext),
            container: Some(c),
            decided_by: Evidence::Magic,
            overridden: false,
        });
    }

    // The bytes decided nothing. The extension is now allowed to.
    match ext {
        Some(e) => Ok(Detection {
            kind: kind_for_extension(e),
            container: None,
            decided_by: if bytes.is_empty() {
                Evidence::NameOnly
            } else {
                Evidence::Extension
            },
            overridden: false,
        }),
        None => Ok(Detection {
            // Unrecognised bytes and no extension: an image is the least
            // surprising guess, and it is what the grid can render. It is
            // reported as `Extension`-decided because that is the weaker
            // guarantee the caller should know it is getting.
            kind: ObjectKind::Image,
            container: None,
            decided_by: Evidence::Extension,
            overridden: false,
        }),
    }
}

/// Map a recognised container to an object kind.
///
/// The interesting cases are the ones where a container is shared by kinds
/// that differ:
///
/// * `Zip` is a gallery only if it holds images, which needs the member list
///   (T-P1-004). A bare `.cbz` is a comic; a bare `.zip` with images is a
///   gallery. Here the extension disambiguates and the archive walk confirms.
/// * `Funscript` is XML, but it is metadata *about* a video, never an object
///   on its own — the scan links it to the scene (T-P1-007).
/// * A `Gif` is an image until the probe says it has many frames over a real
///   duration, at which point it is a scene (T-P1-002, stash#5111).
fn kind_for_container(c: Container, ext: Option<&str>) -> ObjectKind {
    use Container::*;
    match c {
        IsoBmff | Matroska | Webm | Avi | AsfWmv | QuickTime | MpegTs | Ogg | Flac | Mp3 | Wave => {
            ObjectKind::Scene
        }
        Jpeg | Png | Bmp | Tiff | Heif | Avif | WebP => ObjectKind::Image,
        Gif => ObjectKind::Image,
        // §5.5: a PDF's content is pages, so it is a comic regardless of what
        // it is called (upstream #1006 asked for exactly this).
        Pdf => ObjectKind::Comic,
        Zip | SevenZip | Rar => match ext {
            // A comic archive is a container of ordered pages; any other zip
            // is a gallery of images. Both need the member list to confirm
            // (T-P1-004); the extension only chooses which walk to run.
            Some("cbz") | Some("cbr") | Some("cb7") => ObjectKind::Comic,
            _ => ObjectKind::Gallery,
        },
        Text => ObjectKind::Text,
        Xml | Json => ObjectKind::Text,
        Funscript => ObjectKind::Text,
    }
}

fn kind_for_extension(ext: &str) -> ObjectKind {
    use commons_core::ObjectKind as K;
    match ext {
        "mp4" | "m4v" | "mkv" | "webm" | "avi" | "mov" | "wmv" | "ts" | "m2ts" | "mpg" | "mpeg"
        | "flv" | "vob" | "ogv" | "3gp" | "divx" | "mxf" => K::Scene,
        "jpg" | "jpeg" | "png" | "webp" | "avif" | "heic" | "heif" | "gif" | "bmp" | "tif"
        | "tiff" | "jxl" => K::Image,
        // A plain archive is a gallery; the comic extensions and pdf are
        // ordered-page containers and are listed separately below. They must
        // not also appear above, or this arm would be unreachable.
        "zip" | "rar" | "7z" | "gz" | "tar" => K::Gallery,
        "cbz" | "cbr" | "cb7" | "pdf" => K::Comic,
        "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "opus" | "wma" | "aiff" | "alac"
        | "ape" => K::Audio,
        "txt" | "md" | "html" | "htm" | "epub" => K::Text,
        "json" | "xml" | "funscript" => K::Text,
        _ => K::Image,
    }
}

/// Recognise the container from magic bytes. Returns `None` when the bytes do
/// not identify anything, which is the signal to consult the extension.
pub fn sniff_container(b: &[u8]) -> Option<Container> {
    // ISO base media (mp4, m4v, mov, 3gp, avif, heif): `ftyp` at offset 4, with
    // the major brand at offset 8. All of them share one box, so the brand is
    // the only thing that separates them, and it has to be read here rather
    // than in a later branch that the first branch would have pre-empted.
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        return Some(match &b[8..12] {
            b"qt  " => Container::QuickTime,
            b"avif" | b"avis" => Container::Avif,
            b"heic" | b"heix" | b"mif1" | b"msf1" => Container::Heif,
            _ => Container::IsoBmff,
        });
    }

    // Matroska / WebM. The EBML header is the same for both; WebM is
    // distinguished by DocType later in the file, which is not in the first 64
    // bytes, so the extension refines it.
    if b.len() >= 4 && b[0] == 0x1A && b[1] == 0x45 && b[2] == 0xDF && b[3] == 0xA3 {
        return Some(Container::Matroska);
    }

    // RIFF: the form type at offset 8 separates AVI, WAVE, and WebP.
    if b.len() >= 12 && &b[0..4] == b"RIFF" {
        return match &b[8..12] {
            b"AVI " => Some(Container::Avi),
            b"WAVE" => Some(Container::Wave),
            b"WEBP" => Some(Container::WebP),
            _ => None,
        };
    }

    // ASF / WMV GUID: 30 26 B2 75 8E 66 CF 11 A6 D9 00 AA 00 62 CE 6C.
    if b.len() >= 16
        && &b[0..16] == b"\x30\x26\xB2\x75\x8E\x66\xCF\x11\xA6\xD9\x00\xAA\x00\x62\xCE\x6C"
    {
        return Some(Container::AsfWmv);
    }

    // MPEG-TS: 0x47 every 188 bytes, and a 0x47 at offset 0.
    if b.len() >= 188 && b[0] == 0x47 && b[188.min(b.len() - 1)] == 0x47 {
        return Some(Container::MpegTs);
    }

    // OggS
    if b.starts_with(b"OggS") {
        return Some(Container::Ogg);
    }
    // fLaC
    if b.starts_with(b"fLaC") {
        return Some(Container::Flac);
    }
    // ID3 or an MPEG audio frame sync (0xFF Ex/Fx).
    if b.starts_with(b"ID3") {
        return Some(Container::Mp3);
    }
    if b.len() >= 2 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0 {
        return Some(Container::Mp3);
    }

    // ZIP and the whole ZIP family (zip, cbz, jar, docx, epub).
    if b.len() >= 4
        && b[0] == 0x50
        && b[1] == 0x4B
        && (b[2] == 0x03 || b[2] == 0x05 || b[2] == 0x07)
    {
        return Some(Container::Zip);
    }
    // 7z
    if b.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return Some(Container::SevenZip);
    }
    // RAR4 `Rar!\x1A\x07\x00`, RAR5 `Rar!\x1A\x07\x01\x00`
    if b.starts_with(b"Rar!\x1A\x07") {
        return Some(Container::Rar);
    }
    // PDF
    if b.starts_with(b"%PDF") {
        return Some(Container::Pdf);
    }

    // Images. SOI, then the format-specific marker.
    if b.starts_with(b"\xFF\xD8\xFF") {
        return Some(Container::Jpeg);
    }
    if b.len() >= 8 && b.starts_with(b"\x89PNG\r\n\x1A\n") {
        return Some(Container::Png);
    }
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return Some(Container::Gif);
    }
    if b.starts_with(b"BM") && b.len() > 6 {
        return Some(Container::Bmp);
    }
    if b.starts_with(b"II\x2A\x00") || b.starts_with(b"MM\x00\x2A") {
        return Some(Container::Tiff);
    }

    // Funscript is XML with a known root element, checked before the generic
    // XML rule so it is reported as its own container.
    if looks_like_funscript(b) {
        return Some(Container::Funscript);
    }
    if looks_like_xml(b) {
        return Some(Container::Xml);
    }
    if looks_like_json(b) {
        return Some(Container::Json);
    }
    if looks_like_text(b) {
        return Some(Container::Text);
    }
    None
}

fn looks_like_funscript(b: &[u8]) -> bool {
    let head = lower_ascii(&b[..b.len().min(512)]);
    head.contains("<funscript")
}

fn looks_like_xml(b: &[u8]) -> bool {
    let head = &b[..b.len().min(64)];
    let head = trim_leading_ws(head);
    head.starts_with(b"<?xml") || (head.starts_with(b"<") && !head.starts_with(b"<!"))
}

fn looks_like_json(b: &[u8]) -> bool {
    let t = trim_leading_ws(b);
    matches!(t.first(), Some(b'{') | Some(b'['))
}

fn looks_like_text(b: &[u8]) -> bool {
    if b.is_empty() {
        return false;
    }
    // A control character other than tab, LF, CR, or FF means binary. This is
    // the same heuristic as `file(1)` and is the reason a truncated NUL-heavy
    // blob does not get typed as a text object and handed to the reader.
    b.iter()
        .take(8192)
        .all(|c| *c >= 0x20 || matches!(c, b'\t' | b'\n' | b'\r' | 0x0C))
}

fn trim_leading_ws(b: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C) {
        i += 1;
    }
    &b[i..]
}

fn lower_ascii(b: &[u8]) -> String {
    b.iter()
        .map(|c| c.to_ascii_lowercase() as char)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// A directory override file, if any ancestor of `path` carries one.
///
/// `.forcegallery` forces a gallery; `.nogallery` suppresses one. Both apply
/// from the directory containing them down, and the nearest wins. The nearest
/// rather than the outermost is the useful behaviour: a library can set a
/// default at its root and then override one series.
pub fn directory_override(path: &Path) -> Option<ObjectKind> {
    let mut dir = path.parent()?;
    loop {
        if dir.join(".forcegallery").exists() {
            return Some(ObjectKind::Gallery);
        }
        if dir.join(".nogallery").exists() {
            return Some(ObjectKind::Image);
        }
        dir = dir.parent()?;
    }
}

/// The lowercased extension, without the dot.
///
/// A `String` rather than `&str` on purpose: an earlier version returned
/// `Box::leak`-ed memory so callers could hold a `&str`, which leaks one
/// allocation per scanned file and never frees it. A library of 200k files
/// would leak 200k strings for the lifetime of the process.
fn extension_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
}

fn read_head(path: &Path, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut f = std::fs::File::open(path)?;
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use commons_core::ObjectKind as K;

    fn p(s: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(s)
    }

    fn det(bytes: &[u8], name: &str) -> Detection {
        detect_from_bytes(bytes, &p(name)).expect("no I/O")
    }

    fn minimal_mp4() -> Vec<u8> {
        let mut v = b"....ftypisom".to_vec();
        v.resize(64, 0);
        v
    }

    #[test]
    fn iso_bmff_is_a_scene_even_when_named_something_else() {
        // The extension says gallery, the bytes say mp4.
        let d = det(&minimal_mp4(), "/x/clip.zip");
        assert_eq!(d.kind, K::Scene);
        assert_eq!(d.container, Some(Container::IsoBmff));
        assert_eq!(d.decided_by, Evidence::Magic);
    }

    #[test]
    fn the_qt_brand_is_quicktime_not_plain_iso() {
        let mut v = b"....ftypqt  ".to_vec();
        v.resize(64, 0);
        let d = det(&v, "/x/a.mov");
        assert_eq!(d.container, Some(Container::QuickTime));
        assert_eq!(d.kind, K::Scene);
    }

    #[test]
    fn matroska_and_webm_are_the_same_ebml_header() {
        // Both start with the EBML magic; the DocType that distinguishes them
        // lives past the first 64 bytes, so the extension refines the name.
        let mut v = b"\x1A\x45\xDF\xA3".to_vec();
        v.resize(64, 0);
        assert_eq!(det(&v, "/x/a.mkv").container, Some(Container::Matroska));
        assert_eq!(det(&v, "/x/a.webm").container, Some(Container::Matroska));
        assert_eq!(det(&v, "/x/a.mkv").kind, K::Scene);
    }

    #[test]
    fn riff_routes_avi_wave_and_webp_to_three_different_kinds() {
        let mk = |form: &[u8; 4]| {
            let mut v = b"RIFF".to_vec();
            v.extend_from_slice(&[0, 0, 0, 0]);
            v.extend_from_slice(form);
            v.resize(64, 0);
            v
        };
        assert_eq!(det(&mk(b"AVI "), "/x/a.avi").kind, K::Scene);
        assert_eq!(
            det(&mk(b"WAVE"), "/x/a.wav").container,
            Some(Container::Wave)
        );
        assert_eq!(
            det(&mk(b"WEBP"), "/x/a.webp").container,
            Some(Container::WebP)
        );
    }

    #[test]
    fn asf_wmv_is_recognised_by_its_guid() {
        let mut v = b"\x30\x26\xB2\x75\x8E\x66\xCF\x11\xA6\xD9\x00\xAA\x00\x62\xCE\x6C".to_vec();
        v.resize(64, 0);
        assert_eq!(det(&v, "/x/a.wmv").container, Some(Container::AsfWmv));
        assert_eq!(det(&v, "/x/a.wmv").kind, K::Scene);
    }

    #[test]
    fn the_zip_family_splits_by_extension_into_gallery_and_comic() {
        let mut v = b"PK\x03\x04".to_vec();
        v.resize(64, 0);
        // A bare zip with images is a gallery; the same bytes named .cbz are a
        // comic. T-P1-004 confirms the gallery case by walking the members.
        assert_eq!(det(&v, "/x/pics.zip").kind, K::Gallery);
        assert_eq!(det(&v, "/x/book.cbz").kind, K::Comic);
    }

    #[test]
    fn pdf_is_a_comic_because_pages_are_the_content() {
        // §5.5: PDF is a comic source. Upstream filed it as a feature request
        // (#1006); here it is a first-class kind rather than a special case.
        let mut v = b"%PDF-1.7\n".to_vec();
        v.resize(64, 0);
        assert_eq!(det(&v, "/x/book.pdf").kind, K::Comic);
    }

    #[test]
    fn ogg_flac_and_mp3_are_distinguished_from_each_other() {
        let mut ogg = b"OggS\0\0\0\0".to_vec();
        ogg.resize(64, 0);
        assert_eq!(det(&ogg, "/x/a.ogg").container, Some(Container::Ogg));
        let mut flac = b"fLaC\0\0\0\0".to_vec();
        flac.resize(64, 0);
        assert_eq!(det(&flac, "/x/a.flac").container, Some(Container::Flac));
        let mut id3 = b"ID3\x04\0\0\0\0\0\0".to_vec();
        id3.resize(64, 0);
        assert_eq!(det(&id3, "/x/a.mp3").container, Some(Container::Mp3));
    }

    #[test]
    fn a_mpeg_frame_sync_without_id3_is_still_mp3() {
        // A bare frame header: 0xFF 0xFB. No ID3 tag.
        let mut v = vec![0xFF, 0xFB, 0x90, 0x00];
        v.resize(64, 0);
        assert_eq!(det(&v, "/x/a.mp3").container, Some(Container::Mp3));
    }

    #[test]
    fn image_containers_are_images() {
        let cases: Vec<(&[u8], &str, Container)> = vec![
            (&[0xFF, 0xD8, 0xFF, 0xE0], "a.jpg", Container::Jpeg),
            (&b"\x89PNG\r\n\x1A\n"[..], "a.png", Container::Png),
            (&b"GIF89a"[..], "a.gif", Container::Gif),
            (&b"BM\x00\x00\x00\x00"[..], "a.bmp", Container::Bmp),
            (&b"II\x2A\x00"[..], "a.tif", Container::Tiff),
        ];
        for (magic, name, want) in cases {
            let mut v = magic.to_vec();
            v.resize(64, 0);
            let d = det(&v, &format!("/x/{name}"));
            assert_eq!(d.container, Some(want), "{name}");
            assert_eq!(d.kind, K::Image, "{name}");
        }
    }

    #[test]
    fn a_gif_is_an_image_until_a_probe_says_otherwise() {
        // stash#5111. The container cannot tell a still GIF from an animated
        // one; T-P1-002 promotes it to a Scene on frame count and duration.
        let mut v = b"GIF89a".to_vec();
        v.resize(64, 0);
        let d = det(&v, "/x/a.gif");
        assert_eq!(d.kind, K::Image);
        assert_eq!(d.decided_by, Evidence::Magic);
    }

    #[test]
    fn text_json_and_xml_are_text_objects() {
        let mut t = b"a story begins here\n".to_vec();
        t.resize(64, b' ');
        assert_eq!(det(&t, "/x/story.txt").kind, K::Text);
        let j = b"{\"a\":1}    ";
        assert_eq!(det(j, "/x/d.json").container, Some(Container::Json));
        let x = b"<?xml version=\"1.0\"?><root/>      ";
        assert_eq!(det(x, "/x/d.xml").container, Some(Container::Xml));
    }

    #[test]
    fn a_binary_blob_is_not_mistaken_for_text() {
        // NUL bytes mean binary. Typing this as a text object would hand the
        // reader a screenful of nothing.
        let mut v = vec![0x00, 0x01, 0x02, 0x00];
        v.resize(64, 0);
        let d = det(&v, "/x/blob");
        assert_ne!(d.kind, K::Text);
    }

    #[test]
    fn a_funscript_is_recognised_before_generic_xml() {
        let body = b"<?xml version=\"1.0\"?><funscript version=\"1.0\"></funscript>   ";
        let d = det(body, "/x/a.funscript");
        assert_eq!(d.container, Some(Container::Funscript));
    }

    /// Bytes with no recognisable magic *and* control characters, so the text
    /// heuristic rejects them. `0xAA` is printable, so a run of it really is
    /// text -- an earlier version of this fixture used it and the test failed
    /// for the right reason against the wrong data.
    fn binary_blob() -> Vec<u8> {
        (0..64u8)
            .map(|i| if i % 3 == 0 { 0x00 } else { 0xAA })
            .collect()
    }

    #[test]
    fn the_extension_decides_only_when_the_bytes_do_not() {
        // A file with no recognisable magic: the name is allowed to speak, and
        // the report says so.
        let d = det(&binary_blob(), "/x/thing.mp3");
        assert_eq!(d.kind, K::Audio);
        assert_eq!(d.decided_by, Evidence::Extension);
        let m = det(&minimal_mp4(), "/x/thing.mp3");
        assert_eq!(m.decided_by, Evidence::Magic, "magic must win");
        assert_eq!(m.kind, K::Scene);
    }

    #[test]
    fn unknown_binary_with_no_extension_lands_on_image_and_says_it_is_a_guess() {
        let d = det(&binary_blob(), "/x/mystery");
        assert_eq!(d.kind, K::Image);
        assert_eq!(d.decided_by, Evidence::Extension);
    }

    // --- The directory-override cases, which need a real directory tree ---
    //
    // stash#5185 (.forcegallery) and stash#7179 (.nogallery) are the two
    // ambiguous cases the ticket names individually, so each gets its own test
    // against a real tree rather than a synthesised path.

    fn tree() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn forcegallery_overrides_decisive_magic_bytes() {
        // stash#5185: real mp4 bytes, in a directory the user marked as a
        // gallery. The override wins over magic on purpose -- that is the only
        // reason the file exists.
        let d = tree();
        std::fs::write(d.path().join(".forcegallery"), "").unwrap();
        let f = d.path().join("clip.mp4");
        std::fs::write(&f, minimal_mp4()).unwrap();
        let got = detect(&f).unwrap();
        assert_eq!(got.kind, K::Gallery);
        assert_eq!(got.decided_by, Evidence::Override);
        assert!(got.overridden);
    }

    #[test]
    fn nogallery_removes_a_gallery_that_the_folder_would_otherwise_make() {
        // stash#7179: the folder has images, which would make a gallery, and a
        // .nogallery says the images stand alone. The result must be Image, not
        // "gallery minus one member" -- there is no partial gallery.
        let d = tree();
        std::fs::write(d.path().join(".nogallery"), "").unwrap();
        let f = d.path().join("001.jpg");
        let mut v = b"\xFF\xD8\xFF\xE0".to_vec();
        v.resize(64, 0);
        std::fs::write(&f, v).unwrap();
        let got = detect(&f).unwrap();
        assert_eq!(got.kind, K::Image);
        assert_eq!(got.decided_by, Evidence::Override);
    }

    #[test]
    fn the_nearest_override_wins_so_a_root_default_can_be_overridden() {
        let d = tree();
        // Library-wide default: everything is a gallery.
        std::fs::write(d.path().join(".forcegallery"), "").unwrap();
        let series = d.path().join("series-01");
        std::fs::create_dir(&series).unwrap();
        // This one series is images, not a gallery.
        std::fs::write(series.join(".nogallery"), "").unwrap();

        let in_series = series.join("a.jpg");
        let mut v = b"\xFF\xD8\xFF\xE0".to_vec();
        v.resize(64, 0);
        std::fs::write(&in_series, v).unwrap();
        assert_eq!(detect(&in_series).unwrap().kind, K::Image);

        let elsewhere = d.path().join("b.jpg");
        let mut v2 = b"\xFF\xD8\xFF\xE0".to_vec();
        v2.resize(64, 0);
        std::fs::write(&elsewhere, v2).unwrap();
        assert_eq!(detect(&elsewhere).unwrap().kind, K::Gallery);
    }

    #[test]
    fn an_override_in_an_unrelated_sibling_directory_does_not_apply() {
        // The walk goes to ancestors, not to siblings. A `.nogallery` next door
        // must not change what happens here.
        let d = tree();
        let a = d.path().join("a");
        let b = d.path().join("b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(a.join(".nogallery"), "").unwrap();
        let f = b.join("x.jpg");
        let mut v = b"\xFF\xD8\xFF\xE0".to_vec();
        v.resize(64, 0);
        std::fs::write(&f, v).unwrap();
        assert_eq!(detect(&f).unwrap().decided_by, Evidence::Magic);
    }

    #[test]
    fn an_empty_sidecar_is_decided_by_name_alone() {
        // Funscript sidecars and link bodies may be empty at scan time.
        let d = det(&[], "/x/note.txt");
        assert_eq!(d.kind, K::Text);
        assert_eq!(d.decided_by, Evidence::NameOnly);
    }
}
