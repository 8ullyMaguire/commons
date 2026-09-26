//! §8.2 — candidate generation. One test per proposer, in both directions.
//!
//! The ticket's done-when is explicit: "every proposer has both a positive and
//! a negative test — the negative tests are what keep it from firing on
//! everything." That is the harder half and it is the one worth writing, because
//! a candidate generator that fires on everything is indistinguishable from one
//! that does not work until somebody reads the proposals.
//!
//! Each proposer is tested through its *public* entry point and against a real
//! store, because two of the nine read a signal that only the store can hold
//! (`phash_match`, `peer`) and a unit test with a mocked store would pass for
//! reasons that have nothing to do with the proposer.

use commons_core::ProposalSource;
use commons_index::candidates::{self, CandidateContext};
use commons_store::Store;
use std::collections::HashMap;
use uuid::Uuid;

mod common;
use common::store;

use commons_index::candidates::Candidate;

/// Run every proposer and collect what they produced, keyed by source.
///
/// One helper rather than nine calls per test, because the ticket is about the
/// *set* of proposers: a proposer that fires when it should not shows up here as
/// an unexpected entry, which is a much better failure than an assertion in the
/// middle of a specific test.
async fn all_candidates(
    store: &Store,
    ctx: &CandidateContext,
) -> HashMap<ProposalSource, Vec<Candidate>> {
    // `propose_all`, not a loop over `ALL_SOURCES`: a peer's candidate is
    // produced by a per-peer function that `propose(source)` deliberately does
    // not reach, so a loop over the static list silently omits every peer. That
    // is exactly the bug this helper had, and the peer test caught it — which is
    // the argument for testing through the public entry point rather than
    // through the pieces.
    let mut out: HashMap<ProposalSource, Vec<Candidate>> = HashMap::new();
    for c in candidates::propose_all(store, ctx).await.unwrap() {
        out.entry(c.source).or_default().push(c);
    }
    out
}

fn one(map: &HashMap<ProposalSource, Vec<Candidate>>, source: ProposalSource) -> Vec<Candidate> {
    map.get(&source).cloned().unwrap_or_default()
}

// ---- filename (stash #2680, #484) -----------------------------------------

/// A conventional release filename yields a title, a date, and a studio.
#[tokio::test]
async fn filename_proposes_a_title_from_a_conventional_name() {
    let (_d, store) = store().await;
    let ctx = context(&store, "Studio - Title (2021-06-14) [1080p].mkv").await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Filename,
    );
    // Three, not two, and the studio is the part that matters. An implementation
    // that read the title and the date but not the "release - title" shape would
    // pass a test that only looked for those two, and lose the one field a
    // release group's name gives a curator for free.
    assert_eq!(
        cands.len(),
        3,
        "a name with a title, a date and a studio yields three candidates, got {cands:?}"
    );
    let fields: Vec<&str> = cands.iter().map(|c| c.field.as_str()).collect();
    // Date first, and that is not incidental: the date has to be cut out of the
    // string before the title is parsed out of it. Asserted so that a later
    // "tidier" reorder, which would have to invert the parse, fails here first.
    assert_eq!(fields, vec!["date", "title", "studio"], "in that order");
    let studio = cands
        .iter()
        .find(|c| c.field == "studio")
        .expect("a studio");
    assert_eq!(studio.value, serde_json::json!("Studio"));
    let title = cands
        .iter()
        .find(|c| c.field == "title")
        .expect("a title candidate");
    assert_eq!(title.value, serde_json::json!("Studio - Title"));
    assert_eq!(title.source, ProposalSource::Filename);
    assert!(
        title.justification.contains("filename")
            && title.justification.contains("Studio - Title (2021-06-14)"),
        "the justification names the source and the exact string it read, so a 
         user can see what the parser saw: {:?}",
        title.justification
    );

    let date = cands.iter().find(|c| c.field == "date").expect("a date");
    assert_eq!(date.value, serde_json::json!("2021-06-14"));
}

/// A name with no recognisable structure proposes nothing. The negative test.
#[tokio::test]
async fn filename_proposes_nothing_from_an_opaque_name() {
    let (_d, store) = store().await;
    let ctx = context(&store, "a3f9c2e1.mkv").await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Filename,
    );
    assert!(
        cands.is_empty(),
        "a hash as a filename is not a title: {cands:?}"
    );
}

/// A name that is only a resolution and an extension proposes nothing.
///
/// This is the negative test that matters most. `1080p` looks parseable, and a
/// parser that strips it proposes the title "1080p" to every file in a library.
#[tokio::test]
async fn filename_proposes_nothing_from_a_quality_tag() {
    let (_d, store) = store().await;
    let ctx = context(&store, "1080p.mkv").await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Filename,
    );
    assert!(cands.is_empty(), "a resolution is not a title: {cands:?}");
}

// ---- embedded (container tags, EXIF/IPTC; stash #2719) -------------------

#[tokio::test]
async fn embedded_proposes_a_title_from_a_container_tag() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    let file = file(&ctx);
    put_signal(
        &store,
        file,
        "container:title",
        "The Real Title",
        Some("matroska"),
    )
    .await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Embedded,
    );
    assert_eq!(cands.len(), 1, "one tag, one candidate: {cands:?}");
    assert_eq!(cands[0].field, "title");
    assert_eq!(cands[0].value, serde_json::json!("The Real Title"));
    assert_eq!(cands[0].source, ProposalSource::Embedded);
    assert!(
        cands[0].justification.contains("matroska"),
        "the justification names the container the tag came from: {:?}",
        cands[0].justification
    );
}

/// A container with a tag for a field the schema has no place for proposes
/// nothing. Negative: an extractor that proposes whatever it finds fills the
/// object with fields nothing can read.
#[tokio::test]
async fn embedded_ignores_a_tag_for_a_field_it_cannot_store() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_signal(
        &store,
        file(&ctx),
        "container:x264_core_settings",
        "cabac=1 ref=3",
        Some("matroska"),
    )
    .await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Embedded,
    );
    assert!(
        cands.is_empty(),
        "encoder settings are not a field: {cands:?}"
    );
}

/// An empty container tag proposes nothing. Negative: `""` is a real value that
/// many containers carry, and proposing it would blank every title.
#[tokio::test]
async fn embedded_ignores_an_empty_tag() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_signal(
        &store,
        file(&ctx),
        "container:title",
        "   ",
        Some("matroska"),
    )
    .await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Embedded,
    );
    assert!(cands.is_empty(), "an empty tag is not a title: {cands:?}");
}

// ---- phash_match ----------------------------------------------------------

#[tokio::test]
async fn phash_match_proposes_from_a_described_item_with_the_same_hash() {
    let (_d, store) = store().await;
    let described = object(&store, "A Described Item").await;
    let target = object(&store, "placeholder").await;
    put_object_phash(&store, described, "ffff0000ffff0000").await;

    let ctx = ctx_for(&store, target, "clip.mkv").await;
    put_file_phash(&store, target, "ffff0000ffff0000").await;
    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::PhashMatch,
    );
    assert_eq!(
        cands.len(),
        1,
        "one described match, one candidate: {cands:?}"
    );
    assert_eq!(cands[0].value, serde_json::json!("A Described Item"));
    assert!(
        cands[0].justification.contains("phash"),
        "the justification names the signal: {:?}",
        cands[0].justification
    );
}

/// A different phash proposes nothing — the negative test that matters, since a
/// phash matcher that ignores the hash proposes from every item in the library.
#[tokio::test]
async fn phash_match_proposes_nothing_from_a_different_hash() {
    let (_d, store) = store().await;
    let described = object(&store, "A Described Item").await;
    let target = object(&store, "placeholder").await;
    put_object_phash(&store, described, "ffff0000ffff0000").await;

    let ctx = ctx_for(&store, target, "clip.mkv").await;
    put_file_phash(&store, target, "0000ffff0000ffff").await;
    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::PhashMatch,
    );
    assert!(
        cands.is_empty(),
        "a different hash is not a match: {cands:?}"
    );
}

/// An undescribed item with the same hash proposes nothing.
///
/// §8.2 says "the same perceptual hash as a *described* item". Matching an
/// object whose title is unset would propose the empty string, which is the
/// proposer equivalent of a null pointer.
#[tokio::test]
async fn phash_match_ignores_an_undescribed_item() {
    let (_d, store) = store().await;
    let undescribed = object(&store, "").await;
    let target = object(&store, "placeholder").await;
    put_object_phash(&store, undescribed, "ffff0000ffff0000").await;

    let ctx = ctx_for(&store, target, "clip.mkv").await;
    put_file_phash(&store, target, "ffff0000ffff0000").await;
    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::PhashMatch,
    );
    assert!(cands.is_empty(), "there is no value to propose: {cands:?}");
}

// ---- transcript (local ASR, #8) -------------------------------------------

#[tokio::test]
async fn transcript_proposes_keywords_and_a_description() {
    let (_d, store) = store().await;
    let ctx = context(&store, "interview.mkv").await;
    put_signal(
        &store,
        file(&ctx),
        "asr:transcript",
        "we discuss the tide pools at low water and what lives in them",
        None,
    )
    .await;
    put_signal(
        &store,
        file(&ctx),
        "asr:keywords",
        "tide pools,low water,interview",
        None,
    )
    .await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Transcript,
    );
    assert_eq!(cands.len(), 2, "keywords and a description: {cands:?}");
    let tags = cands.iter().find(|c| c.field == "tags").expect("tags");
    assert_eq!(
        tags.value,
        serde_json::json!(["tide pools", "low water", "interview"])
    );
    let desc = cands
        .iter()
        .find(|c| c.field == "description")
        .expect("a description");
    assert!(
        desc.value.as_str().unwrap().contains("tide pools"),
        "the description comes from the words that were said: {:?}",
        desc.value
    );
    assert!(
        desc.justification.contains("audio"),
        "and the justification says the words came from the audio, not from \
         something already in the library: {:?}",
        desc.justification
    );
}

/// A transcript with no content proposes nothing. Negative.
#[tokio::test]
async fn transcript_proposes_nothing_from_a_silent_file() {
    let (_d, store) = store().await;
    let ctx = context(&store, "silence.mkv").await;
    put_signal(&store, file(&ctx), "asr:transcript", "   ", None).await;
    put_signal(&store, file(&ctx), "asr:keywords", "", None).await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::Transcript,
    );
    assert!(cands.is_empty(), "silence says nothing: {cands:?}");
}

// ---- caption (sidecar files) ----------------------------------------------

#[tokio::test]
async fn caption_proposes_a_title_from_a_sidecar() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_signal(
        &store,
        file(&ctx),
        "sidecar:title",
        "Sidecar Title",
        Some("clip.mkv.title.txt"),
    )
    .await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Caption);
    assert_eq!(cands.len(), 1, "{cands:?}");
    assert_eq!(cands[0].value, serde_json::json!("Sidecar Title"));
    assert_eq!(cands[0].source, ProposalSource::Caption);
    assert!(
        cands[0].justification.contains("clip.mkv.title.txt"),
        "the justification names the sidecar file, so a user can see which file 
         to fix: {:?}",
        cands[0].justification
    );
}

/// A sidecar carrying only formatting proposes nothing.
///
/// A `.nfo` or a `.json` with metadata but no title is extremely common, and a
/// parser that proposes "a title" of whitespace fills a library with blanks.
#[tokio::test]
async fn caption_proposes_nothing_from_a_sidecar_with_no_title() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_signal(&store, file(&ctx), "sidecar:plot", "A plot summary.", None).await;
    put_signal(&store, file(&ctx), "sidecar:title", "\n", None).await;

    // A plot *is* a description, and one is proposed. What must not appear is a
    // `title`: this sidecar deliberately has no `title` key, and inventing one
    // from a plot summary is the failure §8.2.1 names -- a plausible-looking
    // value with nothing behind it. The earlier version of this test asserted
    // the sidecar produced nothing at all, which would have been satisfied by a
    // captioner that read nothing.
    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Caption);
    assert!(
        cands.iter().all(|c| c.field != "title"),
        "a sidecar with no title key must not yield a title: {cands:?}"
    );
    let desc = cands.iter().find(|c| c.field == "description");
    assert_eq!(
        desc.map(|c| c.value.clone()),
        Some(serde_json::json!("A plot summary.")),
        "the plot summary is a description"
    );
}

// ---- ml:tagger ------------------------------------------------------------

#[tokio::test]
async fn ml_tagger_proposes_confident_tags_in_its_namespace() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mp4").await;
    put_typed_signal(
        &store,
        file(&ctx),
        "ml:tag",
        "pose:doggy",
        0.91,
        "censorlab-v3",
    )
    .await;
    put_typed_signal(
        &store,
        file(&ctx),
        "ml:tag",
        "scenario:outdoors",
        0.78,
        "censorlab-v3",
    )
    .await;

    // One candidate, not one per tag, and that is the design rather than a
    // shortcut. `tags` is a list-valued field: a tagger's output is a set, and
    // proposing each tag separately would put N values in the queue for a single
    // inference, N-1 of which a curator must reject to accept the Nth. One
    // proposal that can be taken or left as a whole is also the only form in
    // which the reported *minimum* confidence is meaningful -- the weakest tag is
    // what puts doubt on the set, so the set's confidence is that weakest tag.
    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::MlTagger,
    );
    assert_eq!(cands.len(), 1, "one candidate for one inference: {cands:?}");
    let tag = &cands[0];
    assert_eq!(tag.field, "tags");
    assert_eq!(tag.source, ProposalSource::MlTagger);
    assert_eq!(
        tag.value,
        serde_json::json!(["pose:doggy", "scenario:outdoors"]),
        "both tags, sorted, as one list"
    );
    // The minimum, not the mean and not the first. A set whose weakest member is
    // 0.78 is a set a curator should be shown as 0.78 confident.
    assert_eq!(
        tag.confidence,
        Some(0.78),
        "the set is as confident as its least confident tag"
    );
    assert!(
        tag.justification.contains("censorlab-v3"),
        "and the justification names the model, because a tag a user cannot \
         trace to a model is a tag they cannot distrust: {:?}",
        tag.justification
    );
    assert!(
        tag.justification.contains("0.78"),
        "and the confidence is visible, per §8.2.1 -- not as 0.91, which is \
         what a mean of the two tags would have said: {:?}",
        tag.justification
    );
}

/// A low-confidence tag is not proposed. Negative, and the sharp one: a tagger
/// that proposes everything proposes noise, and §8.2.1's promise is that amateur
/// content gets curated — not that it gets flooded.
#[tokio::test]
async fn ml_tagger_drops_a_tag_below_the_confidence_floor() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mp4").await;
    put_typed_signal(
        &store,
        file(&ctx),
        "ml:tag",
        "pose:guessing",
        0.11,
        "censorlab-v3",
    )
    .await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::MlTagger,
    );
    assert!(
        cands.is_empty(),
        "an 11% confidence is not a proposal: {cands:?}"
    );
}

/// A model output with no confidence is not proposed. Negative: an extractor
/// that cannot say how sure it is has said nothing useful.
#[tokio::test]
async fn ml_tagger_drops_a_tag_with_no_confidence() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mp4").await;
    put_signal(
        &store,
        file(&ctx),
        "ml:tag",
        "pose:unsure",
        Some("censorlab-v3"),
    )
    .await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::MlTagger,
    );
    assert!(
        cands.is_empty(),
        "a tag with no confidence says nothing: {cands:?}"
    );
}

// ---- ml:captioner ---------------------------------------------------------

#[tokio::test]
async fn ml_captioner_proposes_a_title_and_a_description() {
    let (_d, store) = store().await;
    let ctx = context(&store, "frame.jpg").await;
    put_typed_signal(
        &store,
        file(&ctx),
        "ml:caption",
        "two people on a rocky shore",
        0.84,
        "qwen-vl",
    )
    .await;
    put_typed_signal(
        &store,
        file(&ctx),
        "ml:title",
        "Tide pools at low water",
        0.66,
        "qwen-vl",
    )
    .await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::MlCaptioner,
    );
    assert_eq!(cands.len(), 2, "a caption and a title: {cands:?}");
    let desc = cands
        .iter()
        .find(|c| c.field == "description")
        .expect("a description");
    assert!(desc.value.as_str().unwrap().contains("rocky shore"));
    let title = cands.iter().find(|c| c.field == "title").expect("a title");
    assert!(title.value.as_str().unwrap().contains("Tide pools"));
}

/// A caption below the floor proposes nothing. Negative.
#[tokio::test]
async fn ml_captioner_drops_a_caption_below_the_floor() {
    let (_d, store) = store().await;
    let ctx = context(&store, "frame.jpg").await;
    put_typed_signal(&store, file(&ctx), "ml:caption", "a blur", 0.05, "qwen-vl").await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::MlCaptioner,
    );
    assert!(
        cands.is_empty(),
        "5% confidence is not a caption: {cands:?}"
    );
}

// ---- peer:<id> (§13) ------------------------------------------------------

#[tokio::test]
async fn a_peer_proposes_its_settled_value() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    let peer = upsert_peer(&store, "fed.example").await;
    put_peer_value(
        &store,
        &peer,
        ctx.object_id,
        "title",
        "\"Their Title\"",
        0.9,
    )
    .await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Peer);
    assert_eq!(cands.len(), 1, "{cands:?}");
    assert_eq!(cands[0].value, serde_json::json!("Their Title"));
    assert!(
        cands[0].justification.contains("fed.example"),
        "a peer proposal must name the peer, or a user cannot tell whose value 
         it is: {:?}",
        cands[0].justification
    );
}

/// An unknown peer proposes nothing. Negative, and the one that matters: a
/// proposer that trusts any `peer:<id>` string is a proposer that will accept a
/// value from anybody who can make the system ask.
#[tokio::test]
async fn an_unknown_peer_proposes_nothing() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Peer);
    assert!(
        cands.is_empty(),
        "an unrecognised peer has no standing: {cands:?}"
    );
}

/// A peer proposes only the fields it has actually settled. Negative for
/// over-reach: a peer sending an empty title is not proposing an empty title.
#[tokio::test]
async fn a_peer_proposes_nothing_for_an_empty_value() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    let peer = upsert_peer(&store, "fed.example").await;
    put_peer_value(&store, &peer, ctx.object_id, "title", "  ", 0.9).await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Peer);
    assert!(
        cands.is_empty(),
        "a blank value is not a proposal: {cands:?}"
    );
}

// ---- scraper --------------------------------------------------------------

/// A scrape below the confidence floor proposes nothing.
///
/// The negative test that matters for a scraper: a scrap is whatever some
/// upstream site said, with no trust of its own, so the floor is the only thing
/// standing between a page's metadata and the proposal queue. A scraper that
/// ignored the floor would pass the positive test above unchanged.
///
/// And a scrape with *no* confidence proposes nothing either. That is the
/// stricter half, and it is deliberate: "unknown" is not "high", and a value
/// nobody is willing to put a number on cannot be shown to be above a floor.
#[tokio::test]
async fn scraper_proposes_nothing_below_the_confidence_floor() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_signal_at(
        &store,
        file(&ctx),
        "scrape:title",
        "Low Confidence Title",
        Some("upstream.example"),
        Some(0.10),
    )
    .await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Scraper);
    assert!(cands.is_empty(), "0.10 is under a 0.50 floor: {cands:?}");
}

/// A scrape whose confidence is absent proposes nothing — "unknown" is not
/// "high", and a value nobody will put a number on cannot clear a floor.
#[tokio::test]
async fn scraper_proposes_nothing_without_a_confidence() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_signal(
        &store,
        file(&ctx),
        "scrape:title",
        "Unqualified Title",
        Some("upstream.example"),
    )
    .await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Scraper);
    assert!(cands.is_empty(), "no confidence, no proposal: {cands:?}");
}

#[tokio::test]
async fn scraper_proposes_from_an_upstream_source() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_signal_at(
        &store,
        file(&ctx),
        "scrape:title",
        "Upstream Title",
        Some("upstream.example"),
        Some(0.82),
    )
    .await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Scraper);
    assert_eq!(cands.len(), 1, "{cands:?}");
    assert_eq!(cands[0].value, serde_json::json!("Upstream Title"));
    assert!(
        cands[0].justification.contains("upstream.example"),
        "the justification names the upstream, which is the only thing that 
         makes a scraped value checkable: {:?}",
        cands[0].justification
    );
}

/// A scraped value below the confidence floor is not proposed. Negative.
#[tokio::test]
async fn scraper_drops_a_value_below_the_floor() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;
    put_typed_signal(
        &store,
        file(&ctx),
        "scrape:title",
        "Maybe This",
        0.2,
        "upstream.example",
    )
    .await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::Scraper);
    assert!(
        cands.is_empty(),
        "a 20% scrape is not a proposal: {cands:?}"
    );
}

// ---- user -----------------------------------------------------------------

#[tokio::test]
async fn a_user_proposal_is_a_proposal() {
    let (_d, store) = store().await;
    let ctx = context(&store, "clip.mkv").await;

    let cands = one(&all_candidates(&store, &ctx).await, ProposalSource::User);
    assert!(
        cands.is_empty(),
        "a user proposal is an act, not a signal: nothing in a file can propose 
         one, so `User` never generates. {} candidates",
        cands.len()
    );
}

// ---- across all nine ------------------------------------------------------

/// Every source in §8.2's table has an implementation, and nothing else does.
///
/// The list is written out rather than derived from `ALL_SOURCES`, because a
/// test that iterates the implementation cannot notice a source that was
/// forgotten. This one can.
#[tokio::test]
async fn the_nine_sources_of_8_2_are_exactly_the_ones_implemented() {
    let expected: Vec<String> = vec![
        "filename".to_string(),
        "embedded".to_string(),
        "phash_match".to_string(),
        "transcript".to_string(),
        "caption".to_string(),
        "ml:tagger".to_string(),
        "ml:captioner".to_string(),
        "user".to_string(),
        "scraper".to_string(),
        "peer".to_string(),
    ];
    let mut got: Vec<String> = candidates::ALL_SOURCES
        .iter()
        .map(|s| s.as_str().to_string())
        .collect();
    // `Peer` is not in `ALL_SOURCES`: it needs a per-peer loop, so `propose_all`
    // reaches it separately. Added here so the count is ten.
    got.push("peer".to_string());
    got.sort();
    let mut want = expected;
    want.sort();
    assert_eq!(got, want, "§8.2's table, against the implemented sources");

    // Separately, and because the two spellings genuinely differ: §8.2 writes
    // `peer:<id>` and the enum is `peer`. The id is per-instance and lives in
    // `proposer_id`. Asserted here so that if the design ever changes -- a
    // `Peer(String)` variant, say -- the change is deliberate rather than
    // incidental, and the asymmetry is written down rather than merely true.
    assert_eq!(ProposalSource::Peer.as_str(), "peer");
    assert!(
        !candidates::ALL_SOURCES.contains(&ProposalSource::Peer),
        "Peer needs a per-peer loop and must stay out of ALL_SOURCES"
    );
}

/// A proposer never proposes for a field it does not own, and never for a
/// subject that is not there. The negative test for the whole module: a
/// candidate generator that fires on everything is worse than one that does not
/// work, because its output has to be read to find out which it is.
#[tokio::test]
async fn an_empty_context_produces_nothing_from_any_proposer() {
    let (_d, store) = store().await;
    let ctx = context(&store, "a.mkv").await;
    // No signals, no phash, no peer value, and an opaque filename.

    let all = all_candidates(&store, &ctx).await;
    let total: usize = all.values().map(|v| v.len()).sum();
    assert_eq!(
        total, 0,
        "a file with no signals produced {total} candidates: {all:?}"
    );
}

/// Every candidate carries a justification, and it is never empty.
///
/// §8.2's whole requirement: the UI shows "title proposed from filename" beside
/// "title proposed by 4 users". A candidate with a blank justification is a
/// candidate the UI cannot explain, which is the same as a candidate with no
/// provenance.
#[tokio::test]
async fn every_candidate_explains_itself() {
    let (_d, store) = store().await;
    let ctx = context(&store, "Studio - Title (2021-06-14).mkv").await;
    put_signal(
        &store,
        file(&ctx),
        "container:title",
        "Embedded",
        Some("matroska"),
    )
    .await;
    put_typed_signal(&store, file(&ctx), "ml:tag", "pose:doggy", 0.91, "m1").await;

    let all = all_candidates(&store, &ctx).await;
    let total: usize = all.values().map(|v| v.len()).sum();
    assert!(
        total > 0,
        "the fixture must produce something, or this asserts nothing"
    );
    for (source, cands) in &all {
        for c in cands {
            assert!(
                !c.justification.trim().is_empty(),
                "{source:?} proposed {} with no justification",
                c.field
            );
            assert!(
                c.justification.chars().count() > 12,
                "{source:?} proposed a justification too short to be a sentence: {:?}",
                c.justification
            );
        }
    }
}

/// A candidate is written to the store with its source, its justification, and a
/// proposer kind that is never `User`. A machine proposal with a `User` kind is a
/// machine impersonating a person, and it is the failure §8.2.1 is written to
/// prevent. A peer's is `Peer` rather than `auto`, because that is the fact a
/// user needs before accepting a value somebody else settled.
#[tokio::test]
async fn a_written_candidate_is_attributed_to_a_machine() {
    let (_d, store) = store().await;
    let ctx = context(&store, "Studio - Title (2021).mkv").await;

    let written = candidates::propose_and_store(&store, &ctx).await.unwrap();
    assert!(written > 0, "the fixture must produce something");

    let rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT field, source, proposer_kind, justification FROM field_proposal
          WHERE subject_id = ?",
    )
    .bind(ctx.object_id.to_string())
    .fetch_all(store.pool())
    .await
    .unwrap();
    assert!(!rows.is_empty(), "propose_and_store wrote nothing");
    for (field, source, kind, justification) in &rows {
        assert_ne!(
            kind, "user",
            "field {field} from {source} was attributed to a person"
        );
        assert!(
            kind == "auto" || kind == "peer",
            "field {field} from {source} was attributed to {kind}, which is \
             neither automatic nor a peer"
        );
        assert!(
            justification
                .as_deref()
                .is_some_and(|j| !j.trim().is_empty()),
            "field {field} from {source} was written with no justification"
        );
    }
}

/// The same signals, twice, produce the same rows. §8.2 runs on every rescan.
#[tokio::test]
async fn candidate_generation_is_idempotent() {
    let (_d, store) = store().await;
    let ctx = context(&store, "Studio - Title (2021).mkv").await;

    candidates::propose_and_store(&store, &ctx).await.unwrap();
    let first: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM field_proposal WHERE subject_id = ?")
        .bind(ctx.object_id.to_string())
        .fetch_one(store.pool())
        .await
        .unwrap();

    // A second pass over the *same* file, which is what a rescan does. The file
    // is not re-inserted: `file` is unique on (object_id, path), and a rescan
    // finds the row rather than creating a second one.
    candidates::propose_and_store(&store, &ctx).await.unwrap();
    let second: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM field_proposal WHERE subject_id = ?")
            .bind(ctx.object_id.to_string())
            .fetch_one(store.pool())
            .await
            .unwrap();

    assert_eq!(
        first, second,
        "a rescan re-proposed and duplicated rows: {first} then {second}"
    );
}

// ---- helpers --------------------------------------------------------------

async fn object(store: &Store, title: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO object (id, kind, title, organized, created_at, updated_at)
         VALUES (?, 'object', ?, 'unreviewed', ?, ?)",
    )
    .bind(id.to_string())
    .bind(if title.is_empty() { None } else { Some(title) })
    .bind(commons_core::ts::now())
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert object");
    id
}

/// The context's file. Every fixture here has one, and the `None` case is
/// covered by `an_object_with_no_file_produces_nothing`.
fn file(ctx: &CandidateContext) -> Uuid {
    ctx.file_id
        .expect("this fixture built its context with a file")
}

async fn context(store: &Store, filename: &str) -> CandidateContext {
    let object_id = object(store, "").await;
    ctx_for(store, object_id, filename).await
}

/// A context for an object that already has a title, so `phash_match` has
/// something to propose *from*.
async fn ctx_for(store: &Store, object_id: Uuid, filename: &str) -> CandidateContext {
    let file_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO file (id, object_id, path, size_bytes, mtime_ns, state)
         VALUES (?, ?, ?, 0, 0, 'present')",
    )
    .bind(file_id.to_string())
    .bind(object_id.to_string())
    .bind(format!("/library/{filename}"))
    .execute(store.pool())
    .await
    .expect("insert file");
    CandidateContext {
        object_id,
        file_id: Some(file_id),
        filename: Some(filename.to_string()),
        // §8.2's "the same perceptual hash as a described item": how far apart
        // two hashes may be and still count as the same item.
        ..Default::default()
    }
}

async fn put_signal(store: &Store, file: Uuid, kind: &str, value: &str, origin: Option<&str>) {
    put_signal_at(store, file, kind, value, origin, None).await
}

/// As `put_signal`, with the confidence a signal may or may not carry.
///
/// The optionality is the point, not laziness: a scraped value's confidence is
/// what the floor is applied to, so a fixture that always supplied one could not
/// express "this scrape said nothing about how sure it was", which is the case
/// the scraper refuses.
async fn put_signal_at(
    store: &Store,
    file: Uuid,
    kind: &str,
    value: &str,
    origin: Option<&str>,
    confidence: Option<f64>,
) {
    sqlx::query(
        "INSERT INTO file_signal (id, file_id, kind, value, origin, confidence, extracted_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(file.to_string())
    .bind(kind)
    .bind(value)
    .bind(origin)
    .bind(confidence)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert file_signal");
}

async fn put_typed_signal(
    store: &Store,
    file: Uuid,
    kind: &str,
    value: &str,
    confidence: f64,
    origin: &str,
) {
    sqlx::query(
        "INSERT INTO file_signal (id, file_id, kind, value, origin, confidence, extracted_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(file.to_string())
    .bind(kind)
    .bind(value)
    .bind(origin)
    .bind(confidence)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert file_signal");
}

async fn put_file_phash(store: &Store, object_id: Uuid, phash: &str) {
    let file_id: String = sqlx::query_scalar("SELECT id FROM file WHERE object_id = ?")
        .bind(object_id.to_string())
        .fetch_one(store.pool())
        .await
        .expect("the file exists");
    sqlx::query(
        "INSERT INTO file_phash (file_id, phash, algorithm, created_at)
         VALUES (?, ?, 'phash64', ?)",
    )
    .bind(file_id)
    .bind(phash)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert file_phash");
}

/// Replace a file's phash, the way a rescan would.
///
/// The unique index on `file_phash` means a second insert for the same file is
/// not possible, so the threshold test has to move the existing row rather than
/// add another. A matcher that kept a *second* hash per file would pass the
/// single-hash tests and fail this one.
async fn put_file_phash_again(store: &Store, object_id: Uuid, phash: &str) {
    let file_id: String = sqlx::query_scalar("SELECT id FROM file WHERE object_id = ?")
        .bind(object_id.to_string())
        .fetch_one(store.pool())
        .await
        .expect("the file exists");
    sqlx::query("UPDATE file_phash SET phash = ? WHERE file_id = ?")
        .bind(phash)
        .bind(file_id)
        .execute(store.pool())
        .await
        .expect("update file_phash");
}

async fn put_object_phash(store: &Store, object_id: Uuid, phash: &str) {
    sqlx::query(
        "INSERT INTO object_phash (object_id, phash, algorithm, created_at)
         VALUES (?, ?, 'phash64', ?)",
    )
    .bind(object_id.to_string())
    .bind(phash)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert object_phash");
}

/// A peer with a display name, which is what a justification has to quote.
async fn upsert_peer(store: &Store, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO peer (id, name, endpoint, public_key, enabled, added_at)
         VALUES (?, ?, ?, 'key', 1, ?)",
    )
    .bind(id.to_string())
    .bind(name)
    .bind(format!("https://{name}"))
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert peer");
    id
}

async fn put_peer_value(
    store: &Store,
    peer: &Uuid,
    object_id: Uuid,
    field: &str,
    value: &str,
    confidence: f64,
) {
    sqlx::query(
        "INSERT INTO peer_value (peer_id, subject_id, field, value_json, confidence, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(peer.to_string())
    .bind(object_id.to_string())
    .bind(field)
    .bind(value)
    .bind(confidence)
    .bind(commons_core::ts::now())
    .execute(store.pool())
    .await
    .expect("insert peer_value");
}

/// The near-match boundary, tested on both sides of it.
///
/// `phash_match_proposes_from_a_described_item_with_the_same_hash` and
/// `..._from_a_different_hash` bracket the threshold from far away: distance 0
/// and distance 64. Both pass with the threshold hard-coded to anything at all,
/// which is what the mutation sweep showed -- `> 0` instead of
/// `> phash_distance` killed nothing.
///
/// So the threshold is pinned here at the boundary, which is the only place a
/// threshold is distinguishable from a constant: one bit apart is a match, two
/// bits apart is not. `ctx_for`'s default is 4, chosen to sit inside the
/// genuinely-different band rather than at either extreme.
#[tokio::test]
async fn phash_match_uses_the_threshold_not_just_the_algorithm() {
    let (_d, store) = store().await;
    let described = object(&store, "A Described Item").await;
    let target = object(&store, "placeholder").await;
    put_object_phash(&store, described, "ffff0000ffff0000").await;

    // The threshold is set explicitly, because the default is 0 -- exact equality
    // -- and that default is right for a real pHash: the same image at two sizes
    // has two different hashes, so a generous threshold proposes near-duplicates
    // as the same clip. This test exists to pin the threshold a caller *can* set,
    // and every other fixture in this file leaves it at 0 -- which is exactly why
    // the mutation `> phash_distance` -> `> 0` killed nothing before it existed.
    let mut ctx = ctx_for(&store, target, "clip.mkv").await;
    ctx.phash_distance = 4;
    assert_eq!(
        CandidateContext::default().phash_distance,
        0,
        "the default is exact-match; this test overrides it deliberately"
    );

    // The hashes are *built at* the distance under test rather than written as hex
    // literals. Counting the bits in a literal by eye is the mistake this test was
    // written to avoid: an earlier version of it used two literals that were both
    // distance 2, and so tested one point twice and called it a boundary.
    put_file_phash(&store, target, &phash_at_distance("ffff0000ffff0000", 2)).await;
    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::PhashMatch,
    );
    assert_eq!(
        cands.len(),
        1,
        "two bits apart is within a threshold of 4: {cands:?}"
    );

    // One bit past the threshold, which is the only distance that tells
    // `> phash_distance` from `> phash_distance + 1` or from `>= 0`.
    put_file_phash_again(&store, target, &phash_at_distance("ffff0000ffff0000", 5)).await;
    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::PhashMatch,
    );
    assert!(
        cands.is_empty(),
        "five bits apart is outside a threshold of 4: {cands:?}"
    );
}

/// A 16-nibble hex hash exactly `bits` bits away from `base`.
///
/// Flips the low `bits` bits, which keeps the result a valid `phash64` and --
/// more to the point -- makes the distance a fact about this constructor rather
/// than a claim about a literal. Asserts on its own output, so a bug here shows up
/// as a wrong distance rather than as a matcher that looks broken.
fn phash_at_distance(base: &str, bits: u32) -> String {
    let value = u64::from_str_radix(base, 16).expect("a hex base");
    assert!(bits <= 64, "a 64-bit hash has 64 bits to differ in");
    let out = format!("{:016x}", value ^ ((1u64 << bits) - 1));
    assert_eq!(
        hamming(base, &out),
        bits,
        "the helper must produce exactly the distance it claims"
    );
    out
}

/// Bits that differ between two hex hashes.
///
/// Deliberately a second implementation rather than the matcher's own function.
/// A shared helper is not a test of it: a wrong hamming that both sides agree on
/// passes every assertion while matching nothing.
fn hamming(a: &str, b: &str) -> u32 {
    let (a, b) = (
        u64::from_str_radix(a, 16).expect("hex a"),
        u64::from_str_radix(b, 16).expect("hex b"),
    );
    (a ^ b).count_ones()
}

/// A file whose own object carries a phash does not propose its own title.
///
/// §8.2's "the same perceptual hash as a described item" means *another* item.
/// An object that holds several files -- the same master and its transcodes --
/// gives every one of them a phash, so a matcher that does not exclude the file's
/// own object proposes the title it already has, to itself, on every rescan.
///
/// The mutation `if object_id == ctx.object_id` -> `if false` killed nothing
/// before this test existed, because every earlier fixture had the described item
/// on a *different* object than the target. This one puts a phash on the
/// target's own object as well, which is the shape a real library has.
#[tokio::test]
async fn phash_match_does_not_propose_an_objects_own_title() {
    let (_d, store) = store().await;
    // One object, holding the file *and* already described.
    let only = object(&store, "Already Described").await;
    put_object_phash(&store, only, "ffff0000ffff0000").await;

    let ctx = ctx_for(&store, only, "clip.mkv").await;
    put_file_phash(&store, only, "ffff0000ffff0000").await;

    let cands = one(
        &all_candidates(&store, &ctx).await,
        ProposalSource::PhashMatch,
    );
    assert!(
        cands.is_empty(),
        "an object's own title is not a candidate for itself: {cands:?}"
    );
}

/// The multi-word form a release tagger actually produces.
///
/// The single-token case is `filename_proposes_nothing_from_a_quality_tag`. This
/// one matters more, because a real release name is never one token: it is
/// `1080p WEB-DL x264 AAC.mkv`, and a quality-token check that compared whole
/// words would see five non-words and propose the lot as a title.
#[tokio::test]
async fn filename_proposes_nothing_from_a_multi_word_quality_tag() {
    let (_d, store) = store().await;
    for name in ["1080p WEB-DL x264 AAC.mkv", "2160p HDR HEVC.mkv"] {
        let ctx = context(&store, name).await;
        let cands = one(
            &all_candidates(&store, &ctx).await,
            ProposalSource::Filename,
        );
        assert!(
            !cands.iter().any(|c| c.field == "title"),
            "{name:?} is a release tag, not a title: {cands:?}"
        );
    }
}
