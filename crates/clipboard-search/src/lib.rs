#![forbid(unsafe_code)]

mod query;
mod ranking;

use std::time::{SystemTime, UNIX_EPOCH};

use clipboard_core::{ContentFlags, ContentKind};
use clipboard_store::{StoreError, StoreHandle};
use rusqlite::{Row, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub use clipboard_store::MAX_PREVIEW_BYTES;
pub use query::{ParsedQuery, QueryError, SearchFilters, parse_query};
pub use ranking::{RankingSignals, RankingWeights, rank_score};

use query::fts_match_expression;

pub const MAX_RANKED_CANDIDATES: usize = 200;
pub const MAX_SEARCH_RESULTS: u32 = 100;
pub const DEFAULT_SEARCH_RESULTS: u32 = 50;

const RANKED_SEARCH_SQL: &str = "WITH matched_candidates AS MATERIALIZED (
       SELECT he.event_id, he.global_id, c.kind, he.captured_at_ms, he.source_app_name,
              he.pinned, c.preview_text, c.byte_size, c.flags, c.content_id,
              bm25(search_fts) AS lexical_score
       FROM search_fts
       JOIN content c ON c.content_id = search_fts.rowid
       JOIN history_event he ON he.content_id = c.content_id
       WHERE search_fts MATCH ?1
         AND (?2 IS NULL OR c.kind = ?2)
         AND (?3 IS NULL
              OR he.source_app_id COLLATE NOCASE = ?3 COLLATE NOCASE
              OR he.source_app_name COLLATE NOCASE = ?3 COLLATE NOCASE)
         AND (?4 IS NULL OR he.pinned = ?4)
       ORDER BY lexical_score, he.captured_at_ms DESC, he.event_id DESC
       LIMIT (?5 + 1)
     ),
     bounded_candidates AS MATERIALIZED (
       SELECT *
       FROM matched_candidates
       ORDER BY lexical_score, captured_at_ms DESC, event_id DESC
       LIMIT ?5
     ),
     candidate_content AS MATERIALIZED (
       SELECT DISTINCT content_id FROM bounded_candidates
     ),
     candidate_usage AS MATERIALIZED (
       SELECT usage.content_id,
              SUM(usage.occurrence_count) AS occurrence_count,
              SUM(usage.paste_count) AS paste_count
       FROM history_event usage
       JOIN candidate_content candidate ON candidate.content_id = usage.content_id
       GROUP BY usage.content_id
     ),
     truncation AS (
       SELECT count(*) > ?5 AS ranked_truncated FROM matched_candidates
     )
     SELECT candidate.event_id, candidate.global_id, candidate.kind,
            candidate.captured_at_ms, candidate.source_app_name, candidate.pinned,
            candidate.preview_text, candidate.byte_size, candidate.flags,
            EXISTS(
              SELECT 1 FROM artifact a
              WHERE a.content_id = candidate.content_id AND a.artifact_kind = 'thumbnail'
            ),
            candidate.lexical_score, usage.occurrence_count, usage.paste_count,
            truncation.ranked_truncated
     FROM bounded_candidates candidate
     JOIN candidate_usage usage ON usage.content_id = candidate.content_id
     CROSS JOIN truncation
     ORDER BY candidate.lexical_score, candidate.captured_at_ms DESC, candidate.event_id DESC";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryCursor {
    pub captured_at_ms: i64,
    pub event_id: i64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchRequest {
    pub query: String,
    pub limit: u32,
    pub cursor: Option<HistoryCursor>,
}

impl SearchRequest {
    pub fn from_text(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: DEFAULT_SEARCH_RESULTS,
            cursor: None,
        }
    }
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self::from_text(String::new())
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub event_id: i64,
    pub global_id: Uuid,
    pub kind: ContentKind,
    pub captured_at_ms: i64,
    pub source_app_name: Option<String>,
    pub pinned: bool,
    pub preview: String,
    pub byte_size: u64,
    pub missing_payload: bool,
    pub has_thumbnail: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    pub next_cursor: Option<HistoryCursor>,
    pub ranked_truncated: bool,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("invalid search query")]
    Query(#[source] QueryError),
    #[error("search limit is invalid")]
    InvalidLimit,
    #[error("ranked search does not support cursors")]
    RankedCursorUnsupported,
    #[error("search storage is unavailable")]
    Store(#[source] StoreError),
    #[error("search storage returned invalid data")]
    InvalidStoreData,
}

impl SearchError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Query(error) => error.code(),
            Self::InvalidLimit => "invalid_limit",
            Self::RankedCursorUnsupported => "ranked_cursor_unsupported",
            Self::Store(_) => "search_unavailable",
            Self::InvalidStoreData => "invalid_store_data",
        }
    }
}

impl From<QueryError> for SearchError {
    fn from(error: QueryError) -> Self {
        Self::Query(error)
    }
}

impl From<StoreError> for SearchError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

pub trait SearchStoreExt {
    fn search(&self, request: SearchRequest) -> Result<HistoryPage, SearchError>;
}

impl SearchStoreExt for StoreHandle {
    fn search(&self, request: SearchRequest) -> Result<HistoryPage, SearchError> {
        if request.limit == 0 || request.limit > MAX_SEARCH_RESULTS {
            return Err(SearchError::InvalidLimit);
        }
        let parsed = parse_query(&request.query)?;
        let match_expression = fts_match_expression(&parsed.text);
        if match_expression.is_empty() {
            return recent_search(self, request.limit, request.cursor, &parsed.filters);
        }
        if request.cursor.is_some() {
            return Err(SearchError::RankedCursorUnsupported);
        }
        ranked_search(
            self,
            request.limit,
            &match_expression,
            &parsed.filters,
            current_time_ms(),
            RankingWeights::default(),
        )
    }
}

fn recent_search(
    store: &StoreHandle,
    limit: u32,
    cursor: Option<HistoryCursor>,
    filters: &SearchFilters,
) -> Result<HistoryPage, SearchError> {
    let kind = filters.kind.map(ContentKind::as_str);
    let app = filters.app.as_deref();
    let pinned = filters.pinned.map(i64::from);
    let cursor_time = cursor.map(|value| value.captured_at_ms);
    let cursor_event = cursor.map(|value| value.event_id);
    let fetch_limit = i64::from(limit) + 1;
    let raw_items = store.with_reader(|connection| {
        let mut statement = connection.prepare(
            "SELECT he.event_id, he.global_id, c.kind, he.captured_at_ms, he.source_app_name,
                    he.pinned, c.preview_text, c.byte_size, c.flags,
                    EXISTS(
                      SELECT 1 FROM artifact a
                      WHERE a.content_id = c.content_id AND a.artifact_kind = 'thumbnail'
                    )
             FROM history_event he
             JOIN content c ON c.content_id = he.content_id
             WHERE (?1 IS NULL OR c.kind = ?1)
               AND (?2 IS NULL
                    OR he.source_app_id COLLATE NOCASE = ?2 COLLATE NOCASE
                    OR he.source_app_name COLLATE NOCASE = ?2 COLLATE NOCASE)
               AND (?3 IS NULL OR he.pinned = ?3)
               AND (?4 IS NULL OR (he.captured_at_ms, he.event_id) < (?4, ?5))
             ORDER BY he.captured_at_ms DESC, he.event_id DESC
             LIMIT ?6",
        )?;
        statement
            .query_map(
                params![kind, app, pinned, cursor_time, cursor_event, fetch_limit],
                raw_history_item,
            )?
            .collect::<Result<Vec<_>, _>>()
    })?;
    let mut items = convert_items(raw_items)?;
    let has_more = items.len() > limit as usize;
    if has_more {
        items.truncate(limit as usize);
    }
    let next_cursor = has_more.then(|| {
        let last = items
            .last()
            .expect("a non-zero page with more rows has an item");
        HistoryCursor {
            captured_at_ms: last.captured_at_ms,
            event_id: last.event_id,
        }
    });
    Ok(HistoryPage {
        items,
        next_cursor,
        ranked_truncated: false,
    })
}

fn ranked_search(
    store: &StoreHandle,
    limit: u32,
    match_expression: &str,
    filters: &SearchFilters,
    now_ms: i64,
    weights: RankingWeights,
) -> Result<HistoryPage, SearchError> {
    let kind = filters.kind.map(ContentKind::as_str);
    let app = filters.app.as_deref();
    let pinned = filters.pinned.map(i64::from);
    let candidate_limit =
        i64::try_from(MAX_RANKED_CANDIDATES).map_err(|_| SearchError::InvalidStoreData)?;
    let (raw_candidates, ranked_truncated) = store.with_reader(|connection| {
        let mut statement = connection.prepare(RANKED_SEARCH_SQL)?;
        let rows = statement
            .query_map(
                params![match_expression, kind, app, pinned, candidate_limit],
                |row| Ok((raw_ranked_item(row)?, row.get::<_, bool>(13)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = rows.first().is_some_and(|(_, truncated)| *truncated);
        let candidates: Vec<RawRankedItem> =
            rows.into_iter().map(|(candidate, _)| candidate).collect();
        Ok((candidates, truncated))
    })?;

    let mut candidates = raw_candidates
        .into_iter()
        .map(|candidate| RankedItem::from_raw(candidate, weights, now_ms))
        .collect::<Result<Vec<_>, _>>()?;
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.item.captured_at_ms.cmp(&left.item.captured_at_ms))
            .then_with(|| right.item.event_id.cmp(&left.item.event_id))
    });
    let items = candidates
        .into_iter()
        .take(limit as usize)
        .map(|candidate| candidate.item)
        .collect();
    Ok(HistoryPage {
        items,
        next_cursor: None,
        ranked_truncated,
    })
}

struct RawHistoryItem {
    event_id: i64,
    global_id: Vec<u8>,
    kind: String,
    captured_at_ms: i64,
    source_app_name: Option<String>,
    pinned: bool,
    preview: String,
    byte_size: i64,
    flags: i64,
    has_thumbnail: bool,
}

struct RawRankedItem {
    item: RawHistoryItem,
    bm25: f64,
    occurrence_count: i64,
    paste_count: i64,
}

struct RankedItem {
    item: HistoryItem,
    score: f64,
}

impl RankedItem {
    fn from_raw(
        raw: RawRankedItem,
        weights: RankingWeights,
        now_ms: i64,
    ) -> Result<Self, SearchError> {
        let occurrence_count =
            u64::try_from(raw.occurrence_count).map_err(|_| SearchError::InvalidStoreData)?;
        let paste_count =
            u64::try_from(raw.paste_count).map_err(|_| SearchError::InvalidStoreData)?;
        let pinned = raw.item.pinned;
        let captured_at_ms = raw.item.captured_at_ms;
        let score = rank_score(
            weights,
            RankingSignals {
                bm25: raw.bm25,
                captured_at_ms,
                occurrence_count,
                paste_count,
                pinned,
            },
            now_ms,
        );
        Ok(Self {
            item: convert_item(raw.item)?,
            score,
        })
    }
}

fn raw_history_item(row: &Row<'_>) -> rusqlite::Result<RawHistoryItem> {
    Ok(RawHistoryItem {
        event_id: row.get(0)?,
        global_id: row.get(1)?,
        kind: row.get(2)?,
        captured_at_ms: row.get(3)?,
        source_app_name: row.get(4)?,
        pinned: row.get(5)?,
        preview: row.get(6)?,
        byte_size: row.get(7)?,
        flags: row.get(8)?,
        has_thumbnail: row.get(9)?,
    })
}

fn raw_ranked_item(row: &Row<'_>) -> rusqlite::Result<RawRankedItem> {
    Ok(RawRankedItem {
        item: raw_history_item(row)?,
        bm25: row.get(10)?,
        occurrence_count: row.get(11)?,
        paste_count: row.get(12)?,
    })
}

fn convert_items(raw_items: Vec<RawHistoryItem>) -> Result<Vec<HistoryItem>, SearchError> {
    raw_items.into_iter().map(convert_item).collect()
}

fn convert_item(raw: RawHistoryItem) -> Result<HistoryItem, SearchError> {
    Ok(HistoryItem {
        event_id: raw.event_id,
        global_id: Uuid::from_slice(&raw.global_id).map_err(|_| SearchError::InvalidStoreData)?,
        kind: content_kind(&raw.kind).ok_or(SearchError::InvalidStoreData)?,
        captured_at_ms: raw.captured_at_ms,
        source_app_name: raw.source_app_name,
        pinned: raw.pinned,
        preview: raw.preview,
        byte_size: u64::try_from(raw.byte_size).map_err(|_| SearchError::InvalidStoreData)?,
        missing_payload: raw.flags & i64::from(ContentFlags::MISSING_PAYLOAD.bits()) != 0,
        has_thumbnail: raw.has_thumbnail,
    })
}

fn content_kind(value: &str) -> Option<ContentKind> {
    match value {
        "text" => Some(ContentKind::Text),
        "link" => Some(ContentKind::Link),
        "image" => Some(ContentKind::Image),
        "file" => Some(ContentKind::File),
        "color" => Some(ContentKind::Color),
        "code" => Some(ContentKind::Code),
        "html" => Some(ContentKind::Html),
        _ => None,
    }
}

fn current_time_ms() -> i64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(millis).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod sql_plan_tests {
    use clipboard_store::{StoreConfig, StoreHandle};
    use rusqlite::params;

    use super::{MAX_RANKED_CANDIDATES, RANKED_SEARCH_SQL};

    #[test]
    fn ranked_sql_materializes_one_bounded_snapshot_and_one_usage_aggregation() {
        let directory = tempfile::tempdir().expect("temporary store");
        let store = StoreHandle::open(StoreConfig::new(directory.path().join("search.sqlite")))
            .expect("synthetic store");
        let details = store
            .with_reader(|connection| {
                let mut statement =
                    connection.prepare(&format!("EXPLAIN QUERY PLAN {RANKED_SEARCH_SQL}"))?;
                statement
                    .query_map(
                        params![
                            "\"synthetic\"",
                            Option::<&str>::None,
                            Option::<&str>::None,
                            Option::<i64>::None,
                            i64::try_from(MAX_RANKED_CANDIDATES).unwrap()
                        ],
                        |row| row.get::<_, String>(3),
                    )?
                    .collect::<Result<Vec<_>, _>>()
            })
            .expect("ranked query plan");

        assert_eq!(
            details
                .iter()
                .filter(|detail| detail.contains("CORRELATED SCALAR SUBQUERY"))
                .count(),
            1,
            "only the thumbnail existence probe may remain correlated"
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("MATERIALIZE bounded_candidates")),
            "bounded candidates must be materialized"
        );
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("MATERIALIZE candidate_usage")),
            "usage must be grouped once for the bounded content set"
        );
    }
}
