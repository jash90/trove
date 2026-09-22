use std::collections::BTreeSet;

use trove_core::{
    CaptureInput, ContentFlags, ContentKind, EventFlags, RepresentationInput, SourceConfidence,
};
use trove_search::{
    HistoryCursor, MAX_APP_FILTER_BYTES, MAX_FTS_MATCH_BYTES, MAX_PREVIEW_BYTES,
    MAX_RANKED_CANDIDATES, MAX_RAW_QUERY_BYTES, MAX_SEARCH_TERM_BYTES, MAX_SEARCH_TERMS,
    RankingSignals, RankingWeights, SearchRequest, SearchStoreExt, parse_query, rank_score,
};
use trove_store::{StoreConfig, StoreHandle};

fn text_capture(
    value: &str,
    captured_at_ms: i64,
    source_app_id: &str,
    source_app_name: &str,
    pinned: bool,
    occurrence_count: u32,
) -> CaptureInput {
    CaptureInput {
        captured_at_ms,
        kind: ContentKind::Text,
        primary_mime: "text/plain".to_owned(),
        representations: vec![RepresentationInput {
            format_id: "public.utf8-plain-text".to_owned(),
            bytes: Some(value.as_bytes().to_vec()),
            missing_ref: None,
        }],
        source_app_id: Some(source_app_id.to_owned()),
        source_app_name: Some(source_app_name.to_owned()),
        source_confidence: SourceConfidence::Declared,
        pinned,
        occurrence_count,
        content_flags: ContentFlags::empty(),
        event_flags: EventFlags::empty(),
        display_label: None,
    }
}

fn request(query: &str, limit: u32, cursor: Option<HistoryCursor>) -> SearchRequest {
    SearchRequest {
        query: query.to_owned(),
        limit,
        cursor,
        include_do_not_index: false,
    }
}

#[test]
fn query_limits_fail_before_secondary_search_allocations_with_stable_codes() {
    assert!(parse_query(&" ".repeat(MAX_RAW_QUERY_BYTES)).is_ok());
    let raw_error = match parse_query(&" ".repeat(MAX_RAW_QUERY_BYTES + 1)) {
        Ok(_) => panic!("oversized raw query unexpectedly parsed"),
        Err(error) => error,
    };
    assert_eq!(raw_error.code(), "query_too_long");

    let app_at_limit = format!("app:{}", "ą".repeat(MAX_APP_FILTER_BYTES / 2));
    assert_eq!(
        parse_query(&app_at_limit)
            .unwrap()
            .filters
            .app
            .unwrap()
            .len(),
        MAX_APP_FILTER_BYTES
    );
    let app_over_limit = format!("app:{}", "ą".repeat(MAX_APP_FILTER_BYTES / 2 + 1));
    let app_error = match parse_query(&app_over_limit) {
        Ok(_) => panic!("oversized app filter unexpectedly parsed"),
        Err(error) => error,
    };
    assert_eq!(app_error.code(), "app_filter_too_long");

    let term_at_limit = "ą".repeat(MAX_SEARCH_TERM_BYTES / 2);
    assert!(
        trove_search::build_search_terms(&term_at_limit)
            .unwrap()
            .fts_expression
            .len()
            <= MAX_FTS_MATCH_BYTES
    );
    let term_over_limit = "ą".repeat(MAX_SEARCH_TERM_BYTES / 2 + 1);
    assert_eq!(
        trove_search::build_search_terms(&term_over_limit)
            .unwrap_err()
            .code(),
        "search_term_too_long"
    );

    let maximum_terms = std::iter::repeat_n("a", MAX_SEARCH_TERMS)
        .collect::<Vec<_>>()
        .join(" ");
    assert!(trove_search::build_search_terms(&maximum_terms).is_ok());
    let too_many_terms = format!("{maximum_terms} a");
    assert_eq!(
        trove_search::build_search_terms(&too_many_terms)
            .unwrap_err()
            .code(),
        "too_many_search_terms"
    );
}

#[test]
fn serde_defaults_diagnostic_history_capability_to_false() {
    let defaulted: SearchRequest = serde_json::from_value(serde_json::json!({
        "query": "",
        "limit": 10,
        "cursor": null
    }))
    .unwrap();
    assert!(!defaulted.include_do_not_index);

    let enabled: SearchRequest = serde_json::from_value(serde_json::json!({
        "query": "",
        "limit": 10,
        "cursor": null,
        "includeDoNotIndex": true
    }))
    .unwrap();
    assert!(enabled.include_do_not_index);
}

#[tokio::test]
async fn a_partial_word_finds_what_the_user_is_still_typing() {
    let (_directory, store) = open_store();
    for (index, text) in ["supercmd i raycast", "zupelnie inny wpis"]
        .into_iter()
        .enumerate()
    {
        store
            .ingest(text_capture(
                text,
                1_000 + index as i64,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap();
    }

    // A palette is typed into one character at a time. Whole-word matching
    // showed nothing until the word was finished, which meant an empty list
    // for most of the typing.
    for prefix in ["su", "super", "supercm", "supercmd"] {
        let page = store
            .search(request(prefix, 10, None))
            .expect("search must succeed");
        assert_eq!(
            page.items.len(),
            1,
            "prefix {prefix:?} should already find the entry"
        );
    }

    let page = store
        .search(request("supercmdx", 10, None))
        .expect("search must succeed");
    assert!(
        page.items.is_empty(),
        "a prefix nothing starts with matches nothing"
    );
}

#[tokio::test]
async fn a_fragment_finds_its_entry_wherever_it_sits_and_whatever_its_case() {
    let (_directory, store) = open_store();
    let texts = ["Supercmd i RayCast", "zupelnie inny wpis"];
    for (index, text) in texts.into_iter().enumerate() {
        store
            .ingest(text_capture(
                text,
                1_000 + index as i64,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap();
    }
    let previews = |query: &str| {
        store
            .search(request(query, 10, None))
            .expect("search must succeed")
            .items
            .into_iter()
            .map(|item| item.preview)
            .collect::<Vec<_>>()
    };

    // What is remembered of a clip is rarely its first letters: the middle of
    // a word, its end, or two pieces of it in whatever order they come to mind.
    for query in [
        "ercm",
        "PERCMD",
        "cmd",
        "aycas",
        "rAYCAST",
        "cast super",
        "cm",
        "YC",
        "c",
        "ray c",
    ] {
        assert_eq!(
            previews(query),
            vec!["Supercmd i RayCast"],
            "{query:?} should find the entry it is a fragment of"
        );
    }
    for query in ["IS", "nny", "elnie s"] {
        assert_eq!(
            previews(query),
            vec!["zupelnie inny wpis"],
            "{query:?} should find the entry it is a fragment of"
        );
    }
    for query in ["cmd is", "xyz", "q"] {
        assert!(
            previews(query).is_empty(),
            "{query:?} is a fragment of no entry"
        );
    }
}

#[tokio::test]
async fn non_indexable_content_is_hidden_from_default_history_but_available_diagnostically() {
    let (_directory, store) = open_store();
    let mut hidden = text_capture(
        "synthetic diagnostic entry",
        2_000,
        "com.example.editor",
        "Example Editor",
        false,
        1,
    );
    hidden.content_flags = ContentFlags::DO_NOT_INDEX;
    store.ingest(hidden).await.unwrap();
    store
        .ingest(text_capture(
            "synthetic visible entry",
            1_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let recent = store.search(request("", 10, None)).unwrap();
    assert_eq!(recent.items.len(), 1);
    assert_eq!(recent.items[0].preview, "synthetic visible entry");

    let mut diagnostic = request("type:text", 10, None);
    diagnostic.include_do_not_index = true;
    let diagnostic = store.search(diagnostic).unwrap();
    assert_eq!(diagnostic.items.len(), 2);

    let mut lexical = request("diagnostic", 10, None);
    lexical.include_do_not_index = true;
    assert!(store.search(lexical).unwrap().items.is_empty());
}

#[test]
fn serde_entry_path_applies_query_and_filter_limits_before_search() {
    let (_directory, store) = open_store();
    let request: SearchRequest = serde_json::from_value(serde_json::json!({
        "query": " ".repeat(MAX_RAW_QUERY_BYTES + 1),
        "limit": 10,
        "cursor": null
    }))
    .unwrap();
    let error = match store.search(request) {
        Ok(_) => panic!("oversized serde query unexpectedly searched"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "query_too_long");
    assert!(!error.to_string().contains(&"x".repeat(32)));
}

fn open_store() -> (tempfile::TempDir, StoreHandle) {
    let directory = tempfile::tempdir().unwrap();
    let store =
        StoreHandle::open(StoreConfig::new(directory.path().join("history.sqlite"))).unwrap();
    (directory, store)
}

#[test]
fn parses_supported_power_filters_at_token_boundaries() {
    let parsed = parse_query("type:text app:\"Example Editor\" is:pinned lodz").unwrap();

    assert_eq!(parsed.text, "lodz");
    assert_eq!(parsed.filters.kind, Some(ContentKind::Text));
    assert_eq!(parsed.filters.app.as_deref(), Some("Example Editor"));
    assert_eq!(parsed.filters.pinned, Some(true));
}

#[test]
fn keeps_unknown_colon_terms_as_lexical_text() {
    let parsed = parse_query("prefix:type:text foo:bar").unwrap();

    assert_eq!(parsed.text, "prefix:type:text foo:bar");
    assert_eq!(parsed.filters.kind, None);
    assert_eq!(parsed.filters.app, None);
    assert_eq!(parsed.filters.pinned, None);
}

#[test]
fn rejects_malformed_and_duplicate_known_filters_with_stable_codes() {
    let cases = [
        ("type:", "invalid_type_filter"),
        ("type:unknown", "invalid_type_filter"),
        ("type:text type:link", "duplicate_type_filter"),
        ("app:", "invalid_app_filter"),
        ("app:\"unterminated", "invalid_app_filter"),
        ("app:One app:Two", "duplicate_app_filter"),
        ("is:", "invalid_is_filter"),
        ("is:false", "invalid_is_filter"),
        ("is:pinned is:pinned", "duplicate_is_filter"),
    ];

    for (query, expected_code) in cases {
        let error = match parse_query(query) {
            Ok(_) => panic!("malformed filter unexpectedly parsed"),
            Err(error) => error,
        };
        assert_eq!(error.code(), expected_code);
        assert!(!error.to_string().contains(query));
    }
}

#[test]
fn ranking_is_deterministic_for_a_fixed_clock() {
    let weights = RankingWeights::default();
    let now_ms = 2_000_000_000_000_i64;
    let baseline = RankingSignals {
        bm25: -2.0,
        captured_at_ms: now_ms - 60_000,
        occurrence_count: 1,
        paste_count: 0,
        pinned: false,
    };
    let frequent = RankingSignals {
        occurrence_count: 20,
        paste_count: 5,
        ..baseline
    };
    let pinned = RankingSignals {
        pinned: true,
        ..baseline
    };

    let first = rank_score(weights, baseline, now_ms);
    assert_eq!(first, rank_score(weights, baseline, now_ms));
    assert!(rank_score(weights, frequent, now_ms) > first);
    assert!(rank_score(weights, pinned, now_ms) > first);
}

#[tokio::test]
async fn normalized_search_is_symmetric_for_composed_and_decomposed_polish_text() {
    let (_directory, store) = open_store();
    store
        .ingest(text_capture(
            "Łódź",
            1_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();
    store
        .ingest(text_capture(
            "LODZ",
            2_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let ascii = store.search(SearchRequest::from_text("lodz")).unwrap();
    let decomposed = store
        .search(SearchRequest::from_text("Ło\u{301}dz\u{301}"))
        .unwrap();

    assert_eq!(ascii.items.len(), 2);
    assert_eq!(decomposed.items.len(), 2);
    assert!(ascii.items.iter().any(|item| item.preview == "Łódź"));
    assert!(decomposed.items.iter().any(|item| item.preview == "LODZ"));
}

#[tokio::test]
async fn fts_metacharacters_are_lexical_tokens_not_query_grammar() {
    let (_directory, store) = open_store();
    store
        .ingest(text_capture(
            "alpha or near quote wildcard punctuation control beta",
            2_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();
    store
        .ingest(text_capture(
            "alpha beta",
            1_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let page = store
        .search(SearchRequest::from_text(
            "alpha OR NEAR \"quote\" wildcard* punctuation!!! control\u{1} beta",
        ))
        .unwrap();

    assert_eq!(page.items.len(), 1);
    assert_eq!(
        page.items[0].preview,
        "alpha or near quote wildcard punctuation control beta"
    );
}

#[tokio::test]
async fn filter_only_search_conjoins_kind_public_app_and_pin() {
    let (_directory, store) = open_store();
    for (name, pinned, timestamp) in [
        ("Example Editor", true, 3_000),
        ("Example Editor", false, 2_000),
        ("Other Editor", true, 1_000),
    ] {
        store
            .ingest(text_capture(
                name,
                timestamp,
                if name == "Example Editor" {
                    "com.example.editor"
                } else {
                    "com.example.other"
                },
                name,
                pinned,
                1,
            ))
            .await
            .unwrap();
    }

    let by_name = store
        .search(request(
            "type:text app:\"Example Editor\" is:pinned",
            10,
            None,
        ))
        .unwrap();
    let by_id = store
        .search(request(
            "type:text app:com.example.editor is:pinned",
            10,
            None,
        ))
        .unwrap();

    assert_eq!(by_name.items.len(), 1);
    assert_eq!(by_name.items[0].preview, "Example Editor");
    assert_eq!(by_id.items.len(), 1);
    assert_eq!(by_id.items[0].event_id, by_name.items[0].event_id);
}

#[tokio::test]
async fn recent_pagination_uses_event_id_for_equal_timestamps() {
    let (_directory, store) = open_store();
    let mut event_ids = Vec::new();
    for value in ["first", "second", "third"] {
        event_ids.push(
            store
                .ingest(text_capture(
                    value,
                    1_000,
                    "com.example.editor",
                    "Example Editor",
                    false,
                    1,
                ))
                .await
                .unwrap()
                .event_id,
        );
    }

    let first = store.search(request("", 2, None)).unwrap();
    let second = store.search(request("", 2, first.next_cursor)).unwrap();

    assert_eq!(first.items.len(), 2);
    assert_eq!(first.items[0].event_id, event_ids[2]);
    assert_eq!(first.items[1].event_id, event_ids[1]);
    assert!(first.next_cursor.is_some());
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].event_id, event_ids[0]);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn recent_cursor_excludes_newer_insertions_but_includes_older_insertions() {
    let (_directory, store) = open_store();
    for (value, timestamp) in [("oldest", 1_000), ("middle", 2_000), ("newest", 3_000)] {
        store
            .ingest(text_capture(
                value,
                timestamp,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap();
    }
    let first = store.search(request("", 2, None)).unwrap();
    store
        .ingest(text_capture(
            "inserted newer",
            4_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();
    store
        .ingest(text_capture(
            "inserted older",
            1_500,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let second = store.search(request("", 10, first.next_cursor)).unwrap();
    let previews = second
        .items
        .iter()
        .map(|item| item.preview.as_str())
        .collect::<Vec<_>>();

    assert_eq!(previews, vec!["inserted older", "oldest"]);
}

#[tokio::test]
async fn ranked_search_rejects_a_recent_mode_cursor() {
    let (_directory, store) = open_store();
    let error = match store.search(request(
        "needle",
        10,
        Some(HistoryCursor {
            captured_at_ms: 1_000,
            event_id: 1,
        }),
    )) {
        Ok(_) => panic!("ranked search unexpectedly accepted a cursor"),
        Err(error) => error,
    };

    assert_eq!(error.code(), "ranked_cursor_unsupported");
    assert!(!error.to_string().contains("needle"));
}

#[tokio::test]
async fn ranked_search_caps_candidates_and_uses_stable_tie_breaks() {
    let (_directory, store) = open_store();
    let mut newest_event_id = 0;
    for index in 0..(MAX_RANKED_CANDIDATES + 5) {
        // Distinct payloads: repeated captures of one content are one group
        // now, and capping candidates is about distinct content. Padded to
        // one length, so every payload is an equal lexical match and the tie
        // breaks are what is left to order them.
        newest_event_id = store
            .ingest(text_capture(
                &format!("shared needle {index:03}"),
                1_000,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap()
            .event_id;
    }

    let page = store.search(request("needle", 25, None)).unwrap();

    assert_eq!(page.items.len(), 25);
    assert_eq!(page.items[0].event_id, newest_event_id);
    assert!(page.ranked_truncated);
    assert!(page.next_cursor.is_none());
    assert!(
        page.items
            .windows(2)
            .all(|pair| pair[0].event_id > pair[1].event_id)
    );
}

#[tokio::test]
async fn timeline_groups_repeated_captures_into_one_row_with_occurrence_data() {
    let (_directory, store) = open_store();
    for index in 0..7 {
        store
            .ingest(text_capture(
                "ta sama notatka",
                1_000 + index,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap();
    }
    store
        .ingest(text_capture(
            "osobny wpis",
            500,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let page = store.search(request("", 10, None)).unwrap();

    assert_eq!(page.items.len(), 2, "one row per distinct content");
    let grouped = &page.items[0];
    assert_eq!(grouped.preview, "ta sama notatka");
    assert_eq!(
        grouped.captured_at_ms, 1_006,
        "the newest occurrence fronts the group"
    );
    assert_eq!(
        grouped.occurrence_count, 5,
        "the cap keeps five recorded occurrences"
    );
    assert_eq!(grouped.occurrences, vec![1_006, 1_005, 1_004, 1_003, 1_002]);
    let single = &page.items[1];
    assert_eq!(single.preview, "osobny wpis");
    assert_eq!(single.occurrence_count, 1);
    assert_eq!(single.occurrences, vec![500]);
}

#[tokio::test]
async fn timeline_cursor_pages_groups_without_repeating_or_losing_one() {
    let (_directory, store) = open_store();
    for (value, timestamp) in [
        ("grupa a", 100),
        ("grupa b", 200),
        ("grupa c", 300),
        ("grupa c", 301),
        ("grupa b", 201),
        ("grupa a", 101),
    ] {
        store
            .ingest(text_capture(
                value,
                timestamp,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap();
    }

    let first = store.search(request("", 2, None)).unwrap();
    let second = store.search(request("", 2, first.next_cursor)).unwrap();
    let mut seen: Vec<String> = first
        .items
        .iter()
        .chain(second.items.iter())
        .map(|item| item.preview.clone())
        .collect();
    seen.sort();

    assert_eq!(first.items.len(), 2);
    assert_eq!(first.items[0].preview, "grupa c");
    assert_eq!(first.items[0].captured_at_ms, 301);
    assert_eq!(second.items.len(), 1);
    assert_eq!(seen, vec!["grupa a", "grupa b", "grupa c"]);
}

#[tokio::test]
async fn pinned_filter_matches_a_group_whose_older_occurrence_is_pinned() {
    let (_directory, store) = open_store();
    store
        .ingest(text_capture(
            "przypięta grupa",
            1_000,
            "com.example.editor",
            "Example Editor",
            true,
            1,
        ))
        .await
        .unwrap();
    for timestamp in [1_001, 1_002] {
        store
            .ingest(text_capture(
                "przypięta grupa",
                timestamp,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap();
    }
    store
        .ingest(text_capture(
            "zwykła grupa",
            1_500,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let pinned = store.search(request("is:pinned", 10, None)).unwrap();
    let timeline = store.search(request("", 10, None)).unwrap();

    assert_eq!(pinned.items.len(), 1);
    assert_eq!(pinned.items[0].preview, "przypięta grupa");
    // The group counts as pinned when any occurrence is pinned, even though
    // the representative fronting it is the newest, unpinned capture.
    let grouped = timeline
        .items
        .iter()
        .find(|item| item.preview == "przypięta grupa")
        .unwrap();
    assert!(grouped.pinned);
}

#[tokio::test]
async fn ranked_search_returns_one_row_per_content_keeping_usage_sums() {
    let (_directory, store) = open_store();
    let mut newest_event_id = 0;
    for index in 0..3 {
        newest_event_id = store
            .ingest(text_capture(
                "needle jeden",
                1_000 + index,
                "com.example.editor",
                "Example Editor",
                false,
                1,
            ))
            .await
            .unwrap()
            .event_id;
    }
    let single = store
        .ingest(text_capture(
            "needle dwa",
            2_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let page = store.search(request("needle", 10, None)).unwrap();

    assert_eq!(page.items.len(), 2, "one row per distinct content");
    let grouped = page
        .items
        .iter()
        .find(|item| item.preview == "needle jeden")
        .unwrap();
    assert_eq!(grouped.event_id, newest_event_id);
    assert_eq!(grouped.occurrence_count, 3);
    assert_eq!(grouped.occurrences, vec![1_002, 1_001, 1_000]);
    let other = page
        .items
        .iter()
        .find(|item| item.preview == "needle dwa")
        .unwrap();
    assert_eq!(other.event_id, single.event_id);
    assert_eq!(other.occurrence_count, 1);
}

#[tokio::test]
async fn list_preview_is_bounded_original_text_and_does_not_read_cas() {
    let (_directory, store) = open_store();
    let original = format!("Łódź {}", "x".repeat(300_000));
    let outcome = store
        .ingest(text_capture(
            &original,
            1_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();
    let blob_relpath = store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT rp.blob_relpath
                 FROM raw_payload rp
                 JOIN event_representation er ON er.raw_payload_id = rp.raw_payload_id
                 JOIN history_event he ON he.event_id = er.event_id
                 WHERE he.content_id = ?1 AND er.ordinal = 0",
                [outcome.content_id],
                |row| row.get::<_, String>(0),
            )
        })
        .unwrap();
    std::fs::remove_file(store.config().blob_root().join(blob_relpath)).unwrap();

    let page = store.search(request("", 10, None)).unwrap();

    assert_eq!(page.items.len(), 1);
    assert!(page.items[0].preview.starts_with("Łódź "));
    assert!(page.items[0].preview.len() <= MAX_PREVIEW_BYTES);
    assert_eq!(page.items[0].byte_size, original.len() as u64);
}

#[tokio::test]
async fn serialized_list_item_exposes_only_the_plan_two_fields() {
    let (_directory, store) = open_store();
    store
        .ingest(text_capture(
            "safe fixture preview",
            1_000,
            "com.example.editor",
            "Example Editor",
            false,
            1,
        ))
        .await
        .unwrap();

    let page = store.search(request("", 10, None)).unwrap();
    let value = serde_json::to_value(&page.items[0]).unwrap();
    let keys = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    assert_eq!(
        keys,
        BTreeSet::from([
            "byteSize",
            "capturedAtMs",
            "eventId",
            "globalId",
            "hasThumbnail",
            "kind",
            "occurrenceCount",
            "occurrences",
            "pinned",
            "preview",
            "sourceAppName",
        ])
    );
    let serialized = value.to_string();
    for forbidden in [
        "inlinePayload",
        "blobRelpath",
        "missingRef",
        "sourceAppOriginal",
        "recordFingerprint",
        "query",
    ] {
        assert!(!serialized.contains(forbidden));
    }
}
