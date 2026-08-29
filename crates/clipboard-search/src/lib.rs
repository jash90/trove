#![forbid(unsafe_code)]

mod query;
mod ranking;

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use clipboard_core::{ContentFlags, ContentKind};
use clipboard_store::{MAX_OCCURRENCES_PER_CONTENT, StoreError, StoreHandle};
use rusqlite::{Row, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub use clipboard_store::MAX_PREVIEW_BYTES;
pub use query::{
    MAX_APP_FILTER_BYTES, MAX_FTS_MATCH_BYTES, MAX_RAW_QUERY_BYTES, MAX_SEARCH_TERM_BYTES,
    MAX_SEARCH_TERMS, ParsedQuery, QueryError, SearchFilters, build_fts_match_expression,
    parse_query,
};
pub use ranking::{RankingSignals, RankingWeights, rank_score};

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
         AND NOT EXISTS (
              SELECT 1 FROM history_event newer
              WHERE newer.content_id = he.content_id
                AND (newer.captured_at_ms, newer.event_id)
                  > (he.captured_at_ms, he.event_id))
       WHERE search_fts MATCH ?1
         AND (c.flags & ?6) = 0
         AND (?2 IS NULL OR c.kind = ?2)
         AND (?3 IS NULL
              OR he.source_app_id COLLATE NOCASE = ?3 COLLATE NOCASE
              OR he.source_app_name COLLATE NOCASE = ?3 COLLATE NOCASE)
         AND (?4 IS NULL
              OR EXISTS(
                   SELECT 1 FROM history_event pinned_member
                   WHERE pinned_member.content_id = c.content_id
                     AND pinned_member.pinned = ?4))
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
     -- CROSS JOIN fixes the order deliberately. A materialized CTE carries no
     -- row estimate, so the planner drove this from history_event and scanned
     -- every row in it to find the handful belonging to the candidates: a fixed
     -- cost that grew with the whole history and, at a million rows, dominated
     -- the search it was attached to. Driving from the candidates instead makes
     -- it as small as the result set.
     candidate_usage AS MATERIALIZED (
       SELECT usage.content_id,
              SUM(usage.occurrence_count) AS occurrence_count,
              SUM(usage.paste_count) AS paste_count
       FROM candidate_content candidate
       CROSS JOIN history_event usage ON usage.content_id = candidate.content_id
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
            truncation.ranked_truncated, candidate.content_id
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
    #[serde(default)]
    pub include_do_not_index: bool,
}

impl SearchRequest {
    pub fn from_text(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            limit: DEFAULT_SEARCH_RESULTS,
            cursor: None,
            include_do_not_index: false,
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
    pub has_thumbnail: bool,
    /// How many captures of this content are recorded, summed over its events.
    pub occurrence_count: u64,
    /// When this content was captured, newest first, capped at the occurrence
    /// limit the store keeps.
    pub occurrences: Vec<i64>,
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
        let match_expression = build_fts_match_expression(&parsed.text)?;
        if match_expression.is_empty() {
            return recent_search(
                self,
                request.limit,
                request.cursor,
                &parsed.filters,
                request.include_do_not_index,
            );
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
    include_do_not_index: bool,
) -> Result<HistoryPage, SearchError> {
    let kind = filters.kind.map(ContentKind::as_str);
    let app = filters.app.as_deref();
    let pinned = filters.pinned.map(i64::from);
    let cursor_time = cursor.map(|value| value.captured_at_ms);
    let cursor_event = cursor.map(|value| value.event_id);
    let fetch_limit = i64::from(limit) + 1;
    let do_not_index_mask = i64::from(ContentFlags::DO_NOT_INDEX.bits());
    let grouped = store.with_reader(|connection| {
        let mut statement = connection.prepare(
            "SELECT he.event_id, he.global_id, c.kind, he.captured_at_ms, he.source_app_name,
                    he.pinned, c.preview_text, c.byte_size, c.flags,
                    EXISTS(
                      SELECT 1 FROM artifact a
                      WHERE a.content_id = c.content_id AND a.artifact_kind = 'thumbnail'
                    ),
                    c.content_id
             FROM history_event he
             JOIN content c ON c.content_id = he.content_id
             WHERE NOT EXISTS (
                    SELECT 1 FROM history_event newer
                    WHERE newer.content_id = he.content_id
                      AND (newer.captured_at_ms, newer.event_id)
                        > (he.captured_at_ms, he.event_id))
               AND (?1 IS NULL OR c.kind = ?1)
               AND (?2 IS NULL
                    OR he.source_app_id COLLATE NOCASE = ?2 COLLATE NOCASE
                    OR he.source_app_name COLLATE NOCASE = ?2 COLLATE NOCASE)
               AND (?3 IS NULL
                    OR EXISTS(
                         SELECT 1 FROM history_event pinned_member
                         WHERE pinned_member.content_id = he.content_id
                           AND pinned_member.pinned = ?3))
               AND (?4 IS NULL OR (he.captured_at_ms, he.event_id) < (?4, ?5))
               AND (?6 = 1 OR (c.flags & ?7) = 0)
             ORDER BY he.captured_at_ms DESC, he.event_id DESC
             LIMIT ?8",
        )?;
        let raw_items = statement
            .query_map(
                params![
                    kind,
                    app,
                    pinned,
                    cursor_time,
                    cursor_event,
                    i64::from(include_do_not_index),
                    do_not_index_mask,
                    fetch_limit
                ],
                |row| raw_history_item(row, 10),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let content_ids = raw_items
            .iter()
            .map(|raw| raw.content_id)
            .collect::<Vec<_>>();
        let fills = occurrence_fills(connection, &content_ids)?;
        Ok((raw_items, fills))
    })?;
    // The page boundary is decided on the rows the query fetched, before any
    // vanished group is dropped: deciding it after would let one deletion
    // between the two reads end the list early even though further groups
    // exist past the boundary.
    let has_more = grouped.0.len() > limit as usize;
    let boundary_cursor = has_more.then(|| {
        let last = grouped
            .0
            .get(limit as usize - 1)
            .expect("a full page has a last row");
        HistoryCursor {
            captured_at_ms: last.captured_at_ms,
            event_id: last.event_id,
        }
    });
    let pairs = grouped
        .0
        .into_iter()
        .take(limit as usize)
        .map(|raw| {
            let content_id = raw.content_id;
            Ok((convert_item(raw)?, content_id))
        })
        .collect::<Result<Vec<_>, SearchError>>()?;
    let items = attach_group_data(pairs, grouped.1);
    Ok(HistoryPage {
        items,
        next_cursor: boundary_cursor,
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
    let do_not_index_mask = i64::from(ContentFlags::DO_NOT_INDEX.bits());
    let ((raw_candidates, ranked_truncated), fills) = store.with_reader(|connection| {
        let mut statement = connection.prepare(RANKED_SEARCH_SQL)?;
        let rows = statement
            .query_map(
                params![
                    match_expression,
                    kind,
                    app,
                    pinned,
                    candidate_limit,
                    do_not_index_mask
                ],
                |row| {
                    Ok((
                        raw_ranked_item(row)?,
                        row.get::<_, bool>(13)?,
                        row.get::<_, i64>(14)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let truncated = rows.first().is_some_and(|(_, truncated, _)| *truncated);
        let candidates: Vec<RawRankedItem> = rows
            .into_iter()
            .map(|(candidate, _, _)| candidate)
            .collect();
        let content_ids = candidates
            .iter()
            .map(|candidate| candidate.item.content_id)
            .collect::<Vec<_>>();
        let fills = occurrence_fills(connection, &content_ids)?;
        Ok(((candidates, truncated), fills))
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
    let pairs = candidates
        .into_iter()
        .take(limit as usize)
        .map(|candidate| (candidate.item, candidate.content_id))
        .collect::<Vec<_>>();
    let items = attach_group_data(pairs, fills);
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
    has_thumbnail: bool,
    content_id: i64,
}

struct RawRankedItem {
    item: RawHistoryItem,
    bm25: f64,
    occurrence_count: i64,
    paste_count: i64,
}

struct RankedItem {
    item: HistoryItem,
    content_id: i64,
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
        let content_id = raw.item.content_id;
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
            content_id,
            score,
        })
    }
}

/// Reads the columns every search row shares. `content_id_index` is where the
/// caller's SELECT put the content identity: the two queries carry different
/// tails, so the position is the caller's to say.
fn raw_history_item(row: &Row<'_>, content_id_index: usize) -> rusqlite::Result<RawHistoryItem> {
    Ok(RawHistoryItem {
        event_id: row.get(0)?,
        global_id: row.get(1)?,
        kind: row.get(2)?,
        captured_at_ms: row.get(3)?,
        source_app_name: row.get(4)?,
        pinned: row.get(5)?,
        preview: row.get(6)?,
        byte_size: row.get(7)?,
        has_thumbnail: row.get(9)?,
        content_id: row.get(content_id_index)?,
    })
}

fn raw_ranked_item(row: &Row<'_>) -> rusqlite::Result<RawRankedItem> {
    Ok(RawRankedItem {
        item: raw_history_item(row, 14)?,
        bm25: row.get(10)?,
        occurrence_count: row.get(11)?,
        paste_count: row.get(12)?,
    })
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
        has_thumbnail: raw.has_thumbnail,
        // Group data is attached afterwards, from one bounded fill query.
        occurrence_count: 0,
        occurrences: Vec::new(),
    })
}

/// Per-content group data: when it was captured, how often, and whether any
/// occurrence is pinned.
struct OccurrenceFill {
    occurrence_count: i64,
    occurrences: Vec<i64>,
    pinned: bool,
}

/// Reads the occurrence rows for one page's contents in a single query.
///
/// The store keeps at most [`MAX_OCCURRENCES_PER_CONTENT`] events per content,
/// so this stays a bounded follow-up: one query for the page instead of a
/// correlated subquery per row.
fn occurrence_fills(
    connection: &rusqlite::Connection,
    content_ids: &[i64],
) -> rusqlite::Result<HashMap<i64, OccurrenceFill>> {
    let mut fills = HashMap::with_capacity(content_ids.len());
    if content_ids.is_empty() {
        return Ok(fills);
    }
    let placeholders = (1..=content_ids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT content_id, captured_at_ms, occurrence_count, pinned
         FROM history_event
         WHERE content_id IN ({placeholders})
         ORDER BY content_id, captured_at_ms DESC, event_id DESC"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        rusqlite::params_from_iter(content_ids.iter().copied()),
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, bool>(3)?,
            ))
        },
    )?;
    for row in rows {
        let (content_id, captured_at_ms, occurrence_count, pinned) = row?;
        let fill = fills.entry(content_id).or_insert(OccurrenceFill {
            occurrence_count: 0,
            occurrences: Vec::new(),
            pinned: false,
        });
        fill.occurrence_count += occurrence_count;
        if fill.occurrences.len() < MAX_OCCURRENCES_PER_CONTENT as usize {
            fill.occurrences.push(captured_at_ms);
        }
        fill.pinned |= pinned;
    }
    Ok(fills)
}

/// Stamps each converted row with its group's data.
///
/// The page query and the fill query are two snapshots on an autocommit WAL
/// reader, so a group deleted in between simply has no fill. It is dropped
/// from the page rather than failing it: a vanished group is not invalid
/// data, and the next request will not see it either.
fn attach_group_data(
    pairs: Vec<(HistoryItem, i64)>,
    mut fills: HashMap<i64, OccurrenceFill>,
) -> Vec<HistoryItem> {
    pairs
        .into_iter()
        .filter_map(|(mut item, content_id)| {
            let fill = fills.remove(&content_id)?;
            item.pinned = fill.pinned;
            item.occurrence_count = u64::try_from(fill.occurrence_count).ok()?;
            item.occurrences = fill.occurrences;
            Some(item)
        })
        .collect()
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
    fn a_group_deleted_between_the_two_reads_is_dropped_not_fatal() {
        use super::{HistoryItem, attach_group_data, occurrence_fills};
        use std::collections::HashMap;

        let directory = tempfile::tempdir().expect("temporary store");
        let store = StoreHandle::open(StoreConfig::new(directory.path().join("search.sqlite")))
            .expect("synthetic store");
        let (kept, vanished) = store
            .with_reader(|connection| {
                let content_ids = [1_i64, 2];
                let fills = occurrence_fills(connection, &content_ids)?;
                Ok((fills.contains_key(&1), fills.contains_key(&2)))
            })
            .unwrap();
        // Neither content exists; the fill map holds nothing for either. The
        // one that exists in the page but not in the fills is a group deleted
        // between the two snapshots, and it leaves the page quietly.
        assert!(!kept && !vanished);
        let item = |event_id: i64| HistoryItem {
            event_id,
            global_id: uuid::Uuid::nil(),
            kind: clipboard_core::ContentKind::Text,
            captured_at_ms: 1_000,
            source_app_name: None,
            pinned: false,
            preview: String::new(),
            byte_size: 0,
            has_thumbnail: false,
            occurrence_count: 0,
            occurrences: Vec::new(),
        };
        let page = attach_group_data(vec![(item(1), 1), (item(2), 2)], HashMap::new());
        assert!(page.is_empty());
    }

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
                            i64::try_from(MAX_RANKED_CANDIDATES).unwrap(),
                            i64::from(clipboard_core::ContentFlags::DO_NOT_INDEX.bits())
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
            3,
            "exactly three probes may remain correlated: the thumbnail \
             existence probe, the representative-event probe, and the \
             pinned-member probe"
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
