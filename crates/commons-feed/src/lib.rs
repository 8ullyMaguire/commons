//! RSS 2.0, Atom 1.0 and JSON Feed 1.1 from one neutral model.
//!
//! # Why this exists as a crate
//!
//! Every service in this estate that publishes a feed builds the XML by hand, because the shape is
//! fixed and small and pulling in a feed library is a dependency with opinions. Hand-building is the
//! right call — it makes the escaping obligation explicit — but it makes it explicit in *each*
//! service, and the obligation is the same one every time. Item 5.4's test is "can a service take
//! this without taking the estate's schema"; this crate has no workspace dependencies at all, so the
//! answer is yes.
//!
//! # The two bugs this crate exists to stop
//!
//! 1. **Unescaped XML.** A post containing `&` or `<` makes the entire feed unparseable, and it fails
//!    at the reader, not at the server — the server returns 200 and a document nobody can use.
//!    [`escape_xml`] escapes the five predefined entities and drops characters that are illegal in
//!    XML 1.0 even when escaped, because escaping cannot save them.
//! 2. **Markdown in a summary.** A description carrying `**bold**` reaches a reader as asterisks.
//!    [`strip_markdown`] flattens it to the plain text a reader can actually show.
//!
//! Both are pure functions with tests, which is the point: a service that uses this crate cannot get
//! them subtly wrong on its own.
//!
//! # What is deliberately not here
//!
//! No fetching, no caching, no conditional GET, no pagination of the feed itself, no HTML rendering
//! of summaries, and no media enclosures. A caller that wants rendered HTML puts it in `summary` and
//! accepts that readers will interpret it (or not) according to their own policy.

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};

/// Channel-level metadata.
#[derive(Debug, Clone, Default)]
pub struct FeedMeta {
    /// Human title, e.g. `WhiteBois` or `WhiteBois — c/rust`.
    pub title: String,
    /// The site's own URL. Absolute; feeds are consumed away from the origin.
    pub link: String,
    /// One line describing the feed. Defaults to the title when absent.
    pub description: Option<String>,
    /// BCP-47 language tag, e.g. `en`.
    pub language: Option<String>,
    /// The feed's own absolute URL, for `atom:link rel="self"`. Readers use it to dedupe subscriptions.
    pub self_url: Option<String>,
}

/// One item. `id` and `link` are separate because they are allowed to differ: an entry's id may be a
/// tag: URI or a permalink, and a reader keys on the id.
#[derive(Debug, Clone)]
pub struct FeedEntry {
    /// Stable identity of the item. Used as `<guid>` and as Atom's `<id>`.
    pub id: String,
    /// Absolute URL.
    pub link: String,
    pub title: String,
    /// Plain text. Markdown is stripped by the renderers, so a caller may pass the stored body.
    pub summary: Option<String>,
    /// Display name of the person who made it.
    pub author: Option<String>,
    pub published: Option<DateTime<Utc>>,
}

/// Escape the five predefined XML entities, and drop characters that are illegal in XML 1.0.
///
/// Dropping rather than escaping is not laziness: `&#0;` is not a valid character reference either,
/// so a control character has no representation. Dropping it keeps the document parseable; keeping it
/// costs the whole feed.
pub fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    for c in input.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Illegal in XML 1.0 even when escaped.
            '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Flatten markdown punctuation to plain text, for a reader that shows text and nothing else.
///
/// This is a deliberate flattening, not a markdown parser: it removes the characters that are only
/// ever markup in practice and leaves the words. A caller that wants structure in a feed should send
/// HTML in `summary` and say so, rather than trusting a stripper to reconstruct it.
///
/// Three rules, each of which was a bug when absent:
///
///   * **links and images lose their target, keep their words** — `[text](url)` → `text`,
///     `![alt](url)` → `alt`. Deleting punctuation alone left `[link](https://…)` inside an XML
///     description, which is what an earlier version of this function did.
///   * **`_` is only markup when it is PAIRED as a marker** — `_emphasis_` and `__strong__` lose it,
///     but `snake_case_name` and `https://x/a_b` keep it. Deleting every underscore silently mangled
///     identifiers and URLs, which is a worse outcome than leaving a stray one.
///   * **line-leading structure goes, indentation stays** — `# heading`, `> quote` and a fenced
///     block's backticks go; the words after them remain.
pub fn strip_markdown(src: &str) -> String {
    // 1. Links and images: keep the label (alt text), drop "(" through the closing ")".
    let mut text = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '[' {
            text.push(c);
            continue;
        }
        // Find the closing bracket, then confirm a "(" follows — otherwise it is just a bracket.
        let rest: String = chars.clone().collect();
        let Some(close) = rest.find(']') else {
            text.push(c);
            continue;
        };
        let label = &rest[..close];
        let after = &rest[close + 1..];
        if !after.starts_with('(') {
            text.push(c);
            continue;
        }
        let Some(paren) = after.find(')') else {
            text.push(c);
            continue;
        };
        // An image is a link with a bang: `![alt](url)`. The bang is punctuation, not words.
        if text.ends_with('!') {
            text.pop();
        }
        text.push_str(label);
        // Consume exactly what was read: the label is only appended here, so skip its characters.
        for _ in 0..(close + 1 + paren + 1) {
            chars.next();
        }
    }

    // 2. Per-character: emphasis/code markers, and "_" only when it is a bit of a pair.
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(chars.len());
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '*' | '`' => {}
            '_' => {
                let prev_word = i > 0 && (chars[i - 1].is_alphanumeric());
                let next_word = chars.get(i + 1).is_some_and(|n| n.is_alphanumeric());
                // `case_name` (word on both sides) keeps its underscore; `_name` / `name_` loses it.
                if prev_word && next_word {
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }

    // 3. Line-leading structure: headings and blockquotes.
    let flattened = out
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('>') {
                rest.trim_start()
            } else {
                let hashes = trimmed.chars().take_while(|c| *c == '#').count();
                if hashes > 0 && trimmed.chars().nth(hashes) == Some(' ') {
                    trimmed[hashes + 1..].trim_start()
                } else {
                    line
                }
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    flattened.trim().to_string()
}

/// Shorten a title to `max` characters, ending in `...` when it was cut.
///
/// Counted in `char`s, not bytes: a 120-byte slice of a title written in Japanese is not a title, and
/// slicing at a byte boundary would panic instead.
pub fn truncate_title(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let keep = max.saturating_sub(3);
    let mut out: String = title.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn rfc2822(t: DateTime<Utc>) -> String {
    t.to_rfc2822()
}

fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn description(meta: &FeedMeta) -> String {
    meta.description
        .clone()
        .unwrap_or_else(|| meta.title.clone())
}

/// RSS 2.0 with an `atom:link rel="self"` when a self URL is known.
pub fn rss2(meta: &FeedMeta, entries: &[FeedEntry]) -> String {
    let base = meta.link.trim_end_matches('/');
    let mut xml = String::with_capacity(4096);
    xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push('\n');
    xml.push_str(r#"<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom">"#);
    xml.push('\n');
    xml.push_str("<channel>\n");
    xml.push_str(&format!(
        "<title>{}</title>\n",
        escape_xml(&meta.title)
    ));
    xml.push_str(&format!("<link>{}</link>\n", escape_xml(&format!("{base}/"))));
    xml.push_str(&format!(
        "<description>{}</description>\n",
        escape_xml(&description(meta))
    ));
    if let Some(lang) = &meta.language {
        xml.push_str(&format!("<language>{}</language>\n", escape_xml(lang)));
    }
    if let Some(self_url) = &meta.self_url {
        xml.push_str(&format!(
            "<atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>\n",
            escape_xml(self_url)
        ));
    }

    for entry in entries {
        xml.push_str("<item>\n");
        xml.push_str(&format!(
            "<title>{}</title>\n",
            escape_xml(&truncate_title(&entry.title, 120))
        ));
        xml.push_str(&format!("<link>{}</link>\n", escape_xml(&entry.link)));
        xml.push_str(&format!(
            "<guid isPermaLink=\"true\">{}</guid>\n",
            escape_xml(&entry.id)
        ));
        if let Some(body) = &entry.summary {
            xml.push_str(&format!(
                "<description>{}</description>\n",
                escape_xml(&strip_markdown(body))
            ));
        }
        if let Some(author) = &entry.author {
            xml.push_str(&format!("<author>{}</author>\n", escape_xml(author)));
        }
        if let Some(published) = entry.published {
            xml.push_str(&format!(
                "<pubDate>{}</pubDate>\n",
                escape_xml(&rfc2822(published))
            ));
        }
        xml.push_str("</item>\n");
    }

    xml.push_str("</channel>\n</rss>\n");
    xml
}

/// Atom 1.0.
///
/// `<updated>` is required by the format and a feed with no items has no entry to borrow one from, so
/// the channel's updated is the newest entry's published time, or the Unix epoch when every entry is
/// undated. Stating the epoch is honest: it says "nothing here has a date", which is true, and it
/// keeps the document valid.
pub fn atom(meta: &FeedMeta, entries: &[FeedEntry]) -> String {
    let updated = entries
        .iter()
        .filter_map(|e| e.published)
        .max()
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch"));
    let mut xml = String::with_capacity(4096);
    xml.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    xml.push('\n');
    xml.push_str(r#"<feed xmlns="http://www.w3.org/2005/Atom">"#);
    xml.push('\n');
    xml.push_str(&format!("<title>{}</title>\n", escape_xml(&meta.title)));
    xml.push_str(&format!(
        "<subtitle>{}</subtitle>\n",
        escape_xml(&description(meta))
    ));
    xml.push_str(&format!("<link href=\"{}\"/>\n", escape_xml(&meta.link)));
    if let Some(self_url) = &meta.self_url {
        xml.push_str(&format!(
            "<link href=\"{}\" rel=\"self\" type=\"application/atom+xml\"/>\n",
            escape_xml(self_url)
        ));
    }
    xml.push_str(&format!("<id>{}</id>\n", escape_xml(&meta.link)));
    xml.push_str(&format!("<updated>{}</updated>\n", rfc3339(updated)));
    if let Some(lang) = &meta.language {
        xml.push_str(&format!("<language>{}</language>\n", escape_xml(lang)));
    }

    for entry in entries {
        xml.push_str("<entry>\n");
        xml.push_str(&format!("<title>{}</title>\n", escape_xml(&entry.title)));
        xml.push_str(&format!("<id>{}</id>\n", escape_xml(&entry.id)));
        xml.push_str(&format!(
            "<link href=\"{}\" rel=\"alternate\"/>\n",
            escape_xml(&entry.link)
        ));
        if let Some(published) = entry.published {
            xml.push_str(&format!(
                "<published>{}</published>\n",
                rfc3339(published)
            ));
            xml.push_str(&format!("<updated>{}</updated>\n", rfc3339(published)));
        }
        if let Some(author) = &entry.author {
            xml.push_str(&format!(
                "<author><name>{}</name></author>\n",
                escape_xml(author)
            ));
        }
        if let Some(body) = &entry.summary {
            xml.push_str(&format!(
                "<summary>{}</summary>\n",
                escape_xml(&strip_markdown(body))
            ));
        }
        xml.push_str("</entry>\n");
    }

    xml.push_str("</feed>\n");
    xml
}

/// JSON Feed 1.1.
///
/// `serde_json` does the JSON escaping, which is the reason to build the document as a `Value` rather
/// than as a string: the one escaping obligation this format has is the one the serializer owns.
pub fn json_feed(meta: &FeedMeta, entries: &[FeedEntry]) -> String {
    let items: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let mut item = json!({
                "id": entry.id,
                "url": entry.link,
                "title": entry.title,
            });
            if let Some(body) = &entry.summary {
                let plain = strip_markdown(body);
                item["summary"] = Value::String(plain.clone());
                item["content_text"] = Value::String(plain);
            }
            if let Some(published) = entry.published {
                item["date_published"] = Value::String(rfc3339(published));
            }
            if let Some(author) = &entry.author {
                item["authors"] = json!([{ "name": author }]);
            }
            item
        })
        .collect();

    let mut doc = json!({
        "version": "https://jsonfeed.org/version/1.1",
        "title": meta.title,
        "home_page_url": meta.link,
        "items": items,
    });
    if let Some(description) = &meta.description {
        doc["description"] = Value::String(description.clone());
    }
    if let Some(self_url) = &meta.self_url {
        doc["feed_url"] = Value::String(self_url.clone());
    }
    if let Some(lang) = &meta.language {
        doc["language"] = Value::String(lang.clone());
    }
    serde_json::to_string_pretty(&doc).expect("a feed document is serialisable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn meta() -> FeedMeta {
        FeedMeta {
            title: "WhiteBois — c/rust".into(),
            link: "https://example.invalid/".into(),
            description: Some("a community feed".into()),
            language: Some("en".into()),
            self_url: Some("https://example.invalid/rss".into()),
        }
    }

    fn entry() -> FeedEntry {
        FeedEntry {
            id: "https://example.invalid/post/42".into(),
            link: "https://example.invalid/post/42".into(),
            title: "hello & <world>".into(),
            summary: Some("# Title\n**bold** `code` a & b".into()),
            author: Some("someone".into()),
            published: Some(Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0).unwrap()),
        }
    }

    #[test]
    fn escapes_the_five_entities() {
        assert_eq!(
            escape_xml(r#"a & b < c > d " e ' f"#),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f"
        );
    }

    /// A control character has no XML representation — `&#0;` is invalid too — so the only way to
    /// keep the document parseable is to drop it.
    #[test]
    fn drops_characters_that_cannot_be_escaped() {
        assert_eq!(escape_xml("a\u{0}b\u{8}c\u{1f}d"), "abcd");
        assert_eq!(escape_xml("héllo — 🎪"), "héllo — 🎪");
    }

    /// The three defects an outside consumer found on 2026-10-29, each pinned by the case that
    /// exposed it. `strip_markdown` used to delete `* _ ` # >` and nothing else, so a link stayed a
    /// link and `snake_case` lost its underscores.
    #[test]
    fn markdown_flattening_keeps_words_and_drops_syntax() {
        // A link keeps its words and loses its target.
        assert_eq!(
            strip_markdown("see [the post](https://x.invalid/p) now"),
            "see the post now"
        );
        // An image keeps its alt text and loses the bang as well as the target.
        assert_eq!(strip_markdown("![alt text](https://x.invalid/i.png)"), "alt text");
        // An underscore BETWEEN word characters is part of a word, not emphasis.
        assert_eq!(
            strip_markdown("call my_fn_name in cfg_a"),
            "call my_fn_name in cfg_a"
        );
        assert_eq!(strip_markdown("https://x.invalid/a_b_c"), "https://x.invalid/a_b_c");
        // A paired underscore IS emphasis and goes.
        assert_eq!(strip_markdown("_emph_ and __strong__"), "emph and strong");
        // Line-leading structure goes; the words stay.
        assert_eq!(strip_markdown("# head\n## sub\nbody"), "head\nsub\nbody");
        assert_eq!(strip_markdown("> quoted\nplain"), "quoted\nplain");
        // A bracket that is not a link is left alone.
        assert_eq!(strip_markdown("array[0] and [draft]"), "array[0] and [draft]");
    }

    #[test]
    fn strips_markdown_to_plain_text() {
        assert_eq!(
            strip_markdown("# Title\n**bold** `code`"),
            "Title\nbold code"
        );
    }

    /// Truncation counts characters, because slicing a non-ASCII title at a byte boundary panics.
    #[test]
    fn truncation_counts_characters_not_bytes() {
        let long = "あ".repeat(200);
        let out = truncate_title(&long, 120);
        assert_eq!(out.chars().count(), 120);
        assert!(out.ends_with("..."));
        assert_eq!(truncate_title("short", 120), "short");
    }

    #[test]
    fn rss_escapes_every_item_field() {
        let xml = rss2(&meta(), &[entry()]);
        assert!(xml.contains(r#"<title>hello &amp; &lt;world&gt;</title>"#), "{xml}");
        assert!(xml.contains("<description>Title\nbold code a &amp; b</description>"), "{xml}");
        assert!(xml.contains(r#"<atom:link href="https://example.invalid/rss" rel="self""#));
        assert!(xml.contains("<pubDate>Thu, 29 Oct 2026 12:00:00 +0000</pubDate>"), "{xml}");
        assert!(!xml.contains("<world>"), "the title was not escaped: {xml}");
    }

    /// The whole document is parseable, not just the fields the test happened to look at. A stray
    /// unescaped `<` anywhere makes every reader reject the feed, and only a parse proves otherwise.
    #[test]
    fn rss_is_well_formed() {
        let xml = rss2(&meta(), &[entry()]);
        let mut reader = quick_xml_stub::Reader::new(&xml);
        reader
            .check()
            .unwrap_or_else(|e| panic!("rss is not well-formed: {e}\n{xml}"));
    }

    /// A minimal well-formedness check, written out rather than pulled in as a dependency: the crate
    /// promises XML, and a dependency-free crate that cannot check its own output is not measuring
    /// the thing it claims.
    mod quick_xml_stub {
        pub struct Reader<'a> {
            input: &'a str,
        }
        impl<'a> Reader<'a> {
            pub fn new(input: &'a str) -> Self {
                Self { input }
            }
            /// Every `<` opens a tag that closes with `>`, every tag is closed or self-closed, and no
            /// bare `&` survives. Text between tags is not inspected beyond that.
            pub fn check(&mut self) -> Result<(), String> {
                let mut stack: Vec<String> = Vec::new();
                let bytes = self.input.as_bytes();
                let mut i = 0;
                while i < bytes.len() {
                    match bytes[i] {
                        b'&' => {
                            let rest = &self.input[i..];
                            let ok = ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"]
                                .iter()
                                .any(|e| rest.starts_with(e));
                            if !ok {
                                return Err(format!("bare & at byte {i}"));
                            }
                            i += 1;
                        }
                        b'<' => {
                            let rest = &self.input[i..];
                            if rest.starts_with(r#"<?xml"#) {
                                let end = rest.find("?>").ok_or("unterminated xml decl")?;
                                i += end + 2;
                                continue;
                            }
                            let end = rest.find('>').ok_or("unterminated tag")?;
                            let tag = &rest[1..end];
                            let closing = tag.starts_with('/');
                            let self_closing = tag.ends_with('/');
                            let name: String = tag
                                .trim_start_matches('/')
                                .trim_end_matches('/')
                                .split_whitespace()
                                .next()
                                .unwrap_or("")
                                .to_string();
                            if closing {
                                match stack.pop() {
                                    Some(open) if open == name => {}
                                    other => {
                                        return Err(format!(
                                            "closing </{name}> does not match {other:?}"
                                        ))
                                    }
                                }
                            } else if !self_closing {
                                stack.push(name);
                            }
                            i += end + 1;
                        }
                        _ => i += 1,
                    }
                }
                if stack.is_empty() {
                    Ok(())
                } else {
                    Err(format!("unclosed tags: {stack:?}"))
                }
            }
        }
    }

    #[test]
    fn atom_is_well_formed_and_carries_the_required_elements() {
        let xml = atom(&meta(), &[entry()]);
        quick_xml_stub::Reader::new(&xml).check().expect("atom xml");
        assert!(xml.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\">"));
        assert!(xml.contains("<updated>2026-10-29T12:00:00Z</updated>"));
        assert!(xml.contains("<id>https://example.invalid/</id>"));
        assert!(xml.contains("<name>someone</name>"));
    }

    /// An undated feed still produces a valid Atom document; the epoch is the honest placeholder.
    #[test]
    fn atom_without_dates_uses_the_epoch_rather_than_omitting_updated() {
        let mut e = entry();
        e.published = None;
        let xml = atom(&meta(), &[e]);
        quick_xml_stub::Reader::new(&xml).check().expect("atom xml");
        assert!(xml.contains("<updated>1970-01-01T00:00:00Z</updated>"), "{xml}");
    }

    #[test]
    fn json_feed_is_json_with_the_version_url() {
        let out = json_feed(&meta(), &[entry()]);
        let doc: Value = serde_json::from_str(&out).expect("json feed parses");
        assert_eq!(doc["version"], "https://jsonfeed.org/version/1.1");
        assert_eq!(doc["title"], "WhiteBois — c/rust");
        assert_eq!(doc["items"][0]["id"], "https://example.invalid/post/42");
        assert_eq!(doc["items"][0]["date_published"], "2026-10-29T12:00:00Z");
        assert_eq!(doc["items"][0]["authors"][0]["name"], "someone");
        // Markdown is stripped here too, so a JSON reader shows what an RSS reader shows.
        assert_eq!(doc["items"][0]["content_text"], "Title\nbold code a & b");
    }

    /// JSON escaping is the serializer's job, and this is the test that says the crate leaned on it
    /// instead of hand-rolling strings: quotes and newlines survive a round trip.
    #[test]
    fn json_feed_survives_quotes_and_newlines() {
        let mut e = entry();
        e.title = "he said \"hi\"\nand left".into();
        let out = json_feed(&meta(), &[e]);
        let doc: Value = serde_json::from_str(&out).expect("json feed parses");
        assert_eq!(doc["items"][0]["title"], "he said \"hi\"\nand left");
    }
}
