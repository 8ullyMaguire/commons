#[path = "harness/mod.rs"]
mod harness;
use harness::{postgres_store, sqlite_store};

use commons_core::domain::Marker;
use commons_store::marker::{
    chapter_interview, insert_marker, markers_for, replace_chapters, ChapterOptions, Word,
};
use uuid::Uuid;

/// Run a block against both engines. See `folders_db.rs` for why this is a
/// macro: `sqlx`'s two result types are unrelated Rust types.
macro_rules! both_engines {
    (|$s:ident| $body:block) => {{
        async {
            let $s = postgres_store().await;
            $body
        }
        .await;
        async {
            let $s = sqlite_store().await;
            $body
        }
        .await;
    }};
}

async fn make_object(store: &commons_store::db::Store, uid: Uuid) -> Uuid {
    let id = uid.to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // Portable upsert, because `INSERT OR REPLACE` is SQLite-only syntax and
    // Postgres refuses it at the `OR` with an offset that names nothing.
    macro_rules! go {
        ($p:expr, $numbered:literal) => {{
            let list: Vec<String> = (1..=3)
                .map(|i| {
                    if $numbered {
                        format!("${i}")
                    } else {
                        "?".to_string()
                    }
                })
                .collect();
            let sql = format!(
                "INSERT INTO object (id, kind, created_at, updated_at)
                 VALUES ({}, 'scene', {}, {})
                 ON CONFLICT (id) DO UPDATE SET updated_at = {}",
                list[0],
                list[1],
                list[2],
                if $numbered {
                    "EXCLUDED.updated_at"
                } else {
                    "excluded.updated_at"
                }
            );
            sqlx::query(&sql)
                .bind(&id)
                .bind(&now)
                .bind(&now)
                .execute($p)
                .await
                .expect("object row");
        }};
    }
    match store {
        commons_store::db::Store::Sqlite(p) => go!(p, false),
        commons_store::db::Store::Postgres(p) => go!(p, true),
    }
    uid
}

fn marker(object: Uuid, title: &str, start: i64, end: Option<i64>) -> Marker {
    Marker {
        id: Uuid::new_v4(),
        object_id: object,
        title: title.to_string(),
        start_ms: start,
        end_ms: end,
        primary_tag_id: None,
        rating: None,
        created_at: chrono::Utc::now().to_rfc3339(),
    }
}

#[tokio::test]
async fn a_marker_is_written_and_read_back_with_its_times() {
    both_engines!(|s| {
        let object = make_object(&s, Uuid::new_v4()).await;
        let m = marker(object, "Cold open", 0, Some(42_000));
        insert_marker(&s, &m).await.expect("write");
        let got = markers_for(&s, &object.to_string()).await.expect("read");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].title, "Cold open");
        assert_eq!(got[0].start_ms, 0);
        assert_eq!(got[0].end_ms, Some(42_000));
        assert_eq!(got[0].id, m.id);
    });
}

#[tokio::test]
async fn markers_come_back_in_time_order_not_insertion_order() {
    both_engines!(|s| {
        // Three rows, distinguishable by their start, inserted out of order. Two
        // rows cannot test an ordering -- they are sorted either way -- so
        // there are three, and the assertion is which one LEADS.
        let object = make_object(&s, Uuid::new_v4()).await;
        for (title, start) in [("third", 30_000), ("first", 0), ("second", 15_000)] {
            insert_marker(&s, &marker(object, title, start, Some(start + 1_000)))
                .await
                .expect("write");
        }
        let got = markers_for(&s, &object.to_string()).await.expect("read");
        assert_eq!(
            got.iter().map(|m| m.title.as_str()).collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
    });
}

#[tokio::test]
async fn an_overlapping_marker_is_refused() {
    both_engines!(|s| {
        // The player's next button would send the user BACKWARDS. A chapter
        // list that renders perfectly and navigates wrongly is the failure.
        let object = make_object(&s, Uuid::new_v4()).await;
        insert_marker(&s, &marker(object, "one", 0, Some(10_000)))
            .await
            .expect("first");
        let err = insert_marker(&s, &marker(object, "two", 5_000, Some(15_000)))
            .await
            .expect_err("an overlap must be refused");
        assert!(err.to_string().contains("overlaps"), "{err}");
        // And nothing was written.
        assert_eq!(markers_for(&s, &object.to_string()).await.unwrap().len(), 1);
    });
}

#[tokio::test]
async fn a_marker_that_abuts_its_neighbour_is_not_an_overlap() {
    both_engines!(|s| {
        // Half-open ranges: [0,10) and [10,20) share no audio, and a check that
        // treated them as overlapping would refuse every chapter in a
        // contiguous chapter list -- which is every chapter list there is.
        //
        // BOTH insertion orders, because the overlap query has two clauses and
        // each order exercises a different one:
        //   existing.start < new.end   and   existing.end > new.start
        // Writing [0,10) then [10,20) tests the second clause. Writing them
        // the other way round -- a chapter that starts where the other ends,
        // inserted FIRST -- tests the first, and a `<` relaxed to `<=` there
        // goes undetected. Confirmed by mutation: with the start clause made
        // inclusive, an earlier draft of this test still passed.
        let forward = make_object(&s, Uuid::new_v4()).await;
        insert_marker(&s, &marker(forward, "one", 0, Some(10_000)))
            .await
            .expect("first");
        insert_marker(&s, &marker(forward, "two", 10_000, Some(20_000)))
            .await
            .expect("abutting markers are not overlapping");
        assert_eq!(
            markers_for(&s, &forward.to_string()).await.unwrap().len(),
            2
        );

        // The same two ranges, second one written first: now `existing.start`
        // equals `new.end`, so only the FIRST clause can be the one refusing it.
        let backward = make_object(&s, Uuid::new_v4()).await;
        insert_marker(&s, &marker(backward, "two", 10_000, Some(20_000)))
            .await
            .expect("the later range, written first");
        insert_marker(&s, &marker(backward, "one", 0, Some(10_000)))
            .await
            .expect("a marker ending exactly where the existing one starts");
        assert_eq!(
            markers_for(&s, &backward.to_string()).await.unwrap().len(),
            2
        );
    });
}

#[tokio::test]
async fn a_marker_with_no_end_runs_to_the_end_of_the_media() {
    both_engines!(|s| {
        // The open-ended form, which is what a "from here on" bookmark is. It
        // overlaps anything that starts after it, and the check has to know
        // that -- a NULL end treated as "no overlap" would let a chapter sit
        // inside another.
        let object = make_object(&s, Uuid::new_v4()).await;
        insert_marker(&s, &marker(object, "open", 5_000, None))
            .await
            .expect("open-ended");
        let err = insert_marker(&s, &marker(object, "later", 10_000, Some(20_000)))
            .await
            .expect_err("a later marker overlaps an open-ended one");
        assert!(err.to_string().contains("overlaps"), "{err}");
    });
}

#[tokio::test]
async fn a_marker_that_cannot_be_seeked_to_is_refused() {
    both_engines!(|s| {
        // A negative start, an end at or before the start, and no title: all
        // three render as a chapter that does nothing.
        let object = make_object(&s, Uuid::new_v4()).await;
        assert!(insert_marker(&s, &marker(object, "x", -1, Some(100)))
            .await
            .is_err());
        assert!(insert_marker(&s, &marker(object, "x", 100, Some(100)))
            .await
            .is_err());
        assert!(insert_marker(&s, &marker(object, "x", 100, Some(50)))
            .await
            .is_err());
        assert!(insert_marker(&s, &marker(object, "  ", 100, Some(200)))
            .await
            .is_err());
        assert!(markers_for(&s, &object.to_string())
            .await
            .unwrap()
            .is_empty());
    });
}

#[tokio::test]
async fn one_objects_markers_do_not_appear_on_another() {
    both_engines!(|s| {
        let a = make_object(&s, Uuid::new_v4()).await;
        let b = make_object(&s, Uuid::new_v4()).await;
        insert_marker(&s, &marker(a, "on-a", 0, Some(1_000)))
            .await
            .unwrap();
        assert!(markers_for(&s, &b.to_string()).await.unwrap().is_empty());
    });
}

#[tokio::test]
async fn a_deleted_object_takes_its_chapters() {
    both_engines!(|s| {
        // The FK has a cascade, and an orphan chapter on a deleted recording is
        // a row that shows up in every chapter list forever.
        let object = make_object(&s, Uuid::new_v4()).await;
        insert_marker(&s, &marker(object, "gone", 0, Some(1_000)))
            .await
            .unwrap();
        macro_rules! del {
            ($c:expr, $numbered:literal) => {{
                let list: Vec<String> = (1..=1)
                    .map(|i| {
                        if $numbered {
                            format!("${i}")
                        } else {
                            "?".to_string()
                        }
                    })
                    .collect();
                let sql = format!("DELETE FROM object WHERE id = {}", list[0]);
                sqlx::query(&sql)
                    .bind(object.to_string())
                    .execute($c)
                    .await
                    .unwrap();
            }};
        }
        match &s {
            commons_store::db::Store::Sqlite(p) => {
                let mut tx = p.begin().await.unwrap();
                del!(&mut *tx, false);
                tx.commit().await.unwrap();
            }
            commons_store::db::Store::Postgres(p) => {
                let mut tx = p.begin().await.unwrap();
                del!(&mut *tx, true);
                tx.commit().await.unwrap();
            }
        }
        assert!(markers_for(&s, &object.to_string())
            .await
            .unwrap()
            .is_empty());
    });
}

// ------------------------------------------------------------- end to end

#[tokio::test]
async fn a_transcript_becomes_a_chapter_list_with_ml_tags() {
    both_engines!(|s| {
        // The whole path: words in, markers out, each tagged through the one
        // API that can write an `ml:` tag. The tag assertion is the load-
        // bearing one -- a chapter tagged `canonical` is a model's opinion filed
        // as a person's, which is the thing §5.15's namespace exists to prevent.
        let object = make_object(&s, Uuid::new_v4()).await;
        let mut words = Vec::new();
        for block in 0..3i64 {
            let base = block * 60_000;
            for i in 0..20 {
                words.push(Word {
                    start_ms: base + i * 1_000,
                    end_ms: base + i * 1_000 + 800,
                    confidence: Some(0.85),
                });
            }
        }
        let opts = ChapterOptions {
            max_ms: 600_000,
            silence_ms: 1_500,
            min_ms: 1_000,
            max_chapters: 100,
        };
        let markers = chapter_interview(
            &s,
            &object,
            &words,
            opts,
            &|index, _chapter| format!("Part {}", index + 1),
            "whisper.cpp",
        )
        .await
        .expect("chapters");
        assert!(markers.len() >= 2, "{}", markers.len());
        let stored = markers_for(&s, &object.to_string()).await.unwrap();
        assert_eq!(stored.len(), markers.len());
        assert_eq!(stored[0].title, "Part 1");
        // Readable, in order, and the ranges do not overlap.
        for pair in stored.windows(2) {
            // `end_ms` is optional on a Marker (an open-ended bookmark has
            // none) but `chapter_interview` always writes one, so the chapters
            // under test are the closed form.
            let prev_end = pair[0].end_ms.expect("a chapter has an end");
            assert!(pair[1].start_ms >= prev_end, "{pair:?}");
        }
    });
}

#[tokio::test]
async fn a_chapter_the_model_has_no_title_for_is_not_written() {
    both_engines!(|s| {
        // A row titled "Chapter 7" that a person then has to clean up is worse
        // than no row: it is a claim that someone chose that title.
        let object = make_object(&s, Uuid::new_v4()).await;
        let words: Vec<Word> = (0..20)
            .map(|i| Word {
                start_ms: i * 1_000,
                end_ms: i * 1_000 + 800,
                confidence: Some(0.9),
            })
            .collect();
        let markers = chapter_interview(
            &s,
            &object,
            &words,
            ChapterOptions {
                min_ms: 1_000,
                ..Default::default()
            },
            &|_index, _chapter| "   ".to_string(),
            "whisper.cpp",
        )
        .await
        .expect("the call succeeds; the chapters are simply not written");
        assert!(
            markers.is_empty(),
            "wrote {} untitled markers",
            markers.len()
        );
        assert!(markers_for(&s, &object.to_string())
            .await
            .unwrap()
            .is_empty());
    });
}

#[tokio::test]
async fn a_re_transcription_replaces_the_chapters_that_described_the_old_one() {
    both_engines!(|s| {
        // Leaving the old chapters means a chapter list whose times no longer
        // match the transcript beside it, and a user who clicks a chapter lands
        // where the previous model put the subject.
        let object = make_object(&s, Uuid::new_v4()).await;
        let old: Vec<Word> = (0..20)
            .map(|i| Word {
                start_ms: i * 1_000,
                end_ms: i * 1_000 + 800,
                confidence: Some(0.9),
            })
            .collect();
        let first = chapter_interview(
            &s,
            &object,
            &old,
            ChapterOptions {
                min_ms: 1_000,
                ..Default::default()
            },
            &|i, _| format!("Old {}", i),
            "whisper.cpp",
        )
        .await
        .expect("first pass");
        assert!(!first.is_empty());

        // The new transcript shifts everything by two minutes.
        let new: Vec<Word> = (0..20)
            .map(|i| Word {
                start_ms: 120_000 + i * 1_000,
                end_ms: 120_000 + i * 1_000 + 800,
                confidence: Some(0.9),
            })
            .collect();
        let mut found = chapter_interview(
            &s,
            &object,
            &new,
            ChapterOptions {
                min_ms: 1_000,
                ..Default::default()
            },
            &|i, _| format!("New {}", i),
            "whisper.cpp",
        )
        .await
        .expect("second pass");
        replace_chapters(&s, &object, &found)
            .await
            .expect("replace");

        let stored = markers_for(&s, &object.to_string()).await.unwrap();
        assert_eq!(stored.len(), found.len(), "the old chapters are gone");
        assert!(
            stored.iter().all(|m| m.title.starts_with("New")),
            "{stored:?}"
        );
        assert!(
            stored[0].start_ms >= 120_000,
            "the new timings, not the old"
        );
        found.clear();
    });
}
