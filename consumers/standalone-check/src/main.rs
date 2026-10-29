//! A consumer that lives OUTSIDE `commons`: if the crates are "usable on their own", this builds and
//! runs with nothing from the estate in its dependency tree.

use commons_feed::{atom, json_feed, rss2, FeedEntry, FeedMeta};
use commons_http::{Cursor, Page, Pagination, Problem, PROBLEM_JSON};
use commons_tokens::{group, token, TOKENS};

fn main() {
    // --- commons-http: an error envelope and a page -------------------------
    let problem = Problem::new(404, "not found").with_detail("no such member");
    println!("problem json: {}", problem.to_json());
    println!("media type: {PROBLEM_JSON} | well-formed: {}", problem.is_well_formed());
    let pagination = Pagination { limit: Some(25), cursor: None };
    println!("page size (default 20, max 100): {}", pagination.page_size(20, 100));
    let cursor = Cursor::new("posts:new", serde_json::json!({"k": "2026-10-29", "id": 7}));
    let wire = cursor.encode();
    println!("cursor url-safe: {} | scope survives: {:?}",
        Cursor::is_url_safe(&wire),
        Cursor::decode(&wire, "posts:new").map(|c| c.scope().to_string()));
    println!("wrong scope refused: {:?}", Cursor::decode(&wire, "posts:top").map(|_| ()).is_err());
    let page = Page::last(vec![1, 2, 3]);
    println!("page items: {:?}", page.items);
    let _ = commons_http::PROBLEM_JSON;

    // --- commons-feed: the same entries in three serialisations -------------
    let meta = FeedMeta {
        title: "congruence".into(),
        link: "https://example.invalid".into(),
        description: Some("standalone check".into()),
        language: Some("en".into()),
        self_url: Some("https://example.invalid/rss".into()),
    };
    let entries = vec![FeedEntry {
        id: "tag:example.invalid,2026:1".into(),
        link: "https://example.invalid/p/1".into(),
        title: "A <title> & an ampersand".into(),
        summary: Some("**bold** summary".into()),
        author: Some("someone".into()),
        published: None,
    }];
    let rss = rss2(&meta, &entries);
    let at = atom(&meta, &entries);
    let jf = json_feed(&meta, &entries);
    println!("rss2 escapes the title: {}", rss.contains("A &lt;title&gt; &amp; an ampersand"));
    println!("bold:      {:?}", commons_feed::strip_markdown("**bold** summary"));
    println!("link:      {:?}", commons_feed::strip_markdown("see [the post](https://x.invalid/p) now"));
    println!("image:     {:?}", commons_feed::strip_markdown("![alt text](https://x.invalid/i.png)"));
    println!("heading:   {:?}", commons_feed::strip_markdown("# head\n## sub\nbody"));
    println!("quote:     {:?}", commons_feed::strip_markdown("> quoted\nplain"));
    println!("snake_case:{:?}", commons_feed::strip_markdown("call my_fn_name in cfg_a"));
    println!("url:       {:?}", commons_feed::strip_markdown("https://x.invalid/a_b_c"));
    println!("emphasis:  {:?}", commons_feed::strip_markdown("_emph_ and __strong__"));
    println!("rss2 body: {:?}", entries.get(0).map(|e| e.summary.clone()));
    println!("atom carries the id:    {}", at.contains("tag:example.invalid,2026:1"));
    println!("json_feed version:      {}", jf.contains("jsonfeed.org/version/1.1"));

    // --- commons-tokens: the Rust side of the design tokens ------------------
    println!("tokens: {} | colour group: {}", TOKENS.len(), group("colour").count());
    println!("lookup by CSS name: {:?}", token("bg").map(|t| t.value));
}
