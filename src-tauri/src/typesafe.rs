//! The privacy scan: one deliberate look through the local history, with
//! the owner's key, for entries that should not be sitting in it.
//!
//! The fourth place this application talks to the network, after link
//! previews, the keyvault and the chat window — and the one with the most
//! to lose, because what it sends is the clipboard history itself. That is
//! why nothing here ever runs on its own: the scan starts when the owner
//! presses the button in settings, sends the *text* of each entry and
//! nothing else — no source application, no timestamps, no identifiers —
//! and its answers never touch the database. A scan's flags live in the
//! scan's own memory; a closed application forgets them, and the history
//! on disk is exactly what it was.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use trove_store::StoreHandle;

/// Where the scan settings live, beside the application settings.
const TYPESAFE_SETTINGS_KEY: &str = "typesafe";
const MAX_API_KEY_BYTES: usize = 4 * 1024;
/// Entries per request: enough to amortize the question definitions,
/// few enough that one slow answer cannot hold a whole scan hostage.
pub const SCAN_BATCH: usize = 20;
/// The ceiling an entry's text may reach on the wire.
const MAX_ENTRY_TEXT_CHARS: usize = 2_000;
/// An entry is flagged at this probability and above. Below it — a bare
/// account number with no context around it, say — the model is honestly
/// unsure, and an unsure flag on someone's history is a false accusation.
pub const FLAG_THRESHOLD: f64 = 0.9;

/// What the scan needs from the settings row: a key, and nothing else.
/// There is no "enabled" flag to keep in step with the key — the scan is
/// a button, not a background behavior, and the button answers only when
/// a key exists.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypesafeSettingsDto {
    #[serde(default)]
    pub api_key: String,
}

pub fn validate_typesafe_settings(settings: &TypesafeSettingsDto) -> Result<(), String> {
    let key = &settings.api_key;
    if key.len() > MAX_API_KEY_BYTES
        || key.trim() != key.as_str()
        || key.chars().any(char::is_control)
    {
        return Err("invalid_typesafe_settings".to_owned());
    }
    Ok(())
}

pub fn read_typesafe_settings_blocking(store: &StoreHandle) -> Result<TypesafeSettingsDto, String> {
    match store.get_setting(TYPESAFE_SETTINGS_KEY) {
        Ok(Some(value_json)) => {
            let settings: TypesafeSettingsDto = serde_json::from_str(&value_json)
                .map_err(|_| "invalid_typesafe_settings".to_owned())?;
            validate_typesafe_settings(&settings)?;
            Ok(settings)
        }
        Ok(None) => Ok(TypesafeSettingsDto::default()),
        Err(_) => Err("typesafe_settings_unavailable".to_owned()),
    }
}

pub async fn get_typesafe_settings_service(
    state: &crate::state::AppState,
) -> Result<TypesafeSettingsDto, String> {
    let store = state.store.clone();
    crate::commands::run_blocking("typesafe_settings_unavailable", move || {
        read_typesafe_settings_blocking(&store)
    })
    .await
}

pub async fn save_typesafe_settings_service(
    state: &crate::state::AppState,
    settings: TypesafeSettingsDto,
) -> Result<TypesafeSettingsDto, String> {
    validate_typesafe_settings(&settings)?;
    let value_json =
        serde_json::to_string(&settings).map_err(|_| "invalid_typesafe_settings".to_owned())?;
    state
        .store
        .save_setting(TYPESAFE_SETTINGS_KEY, &value_json)
        .await
        .map_err(|_| "typesafe_settings_unavailable".to_owned())?;
    Ok(settings)
}

// ------------------------------------------------------------- the scan --

/// One entry the scan looked at and did not like the look of.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedEntryDto {
    pub event_id: i64,
    /// The entry's own text, shown so the owner can judge the flag.
    pub preview: String,
    pub probability: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanState {
    Running,
    Completed,
    Failed,
}

/// Where one scan stands, in the shape the interface polls.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanProgressDto {
    pub run_id: String,
    pub state: ScanState,
    pub processed: usize,
    pub total: usize,
    /// Flagged so far, newest judgment first.
    pub flagged: Vec<FlaggedEntryDto>,
    /// A stable code, when the state is failed.
    pub error_code: Option<String>,
}

struct ScanRun {
    progress: ScanProgressDto,
    cancel: Arc<AtomicBool>,
}

fn scan_slot() -> &'static Mutex<Option<ScanRun>> {
    static SCAN: OnceLock<Mutex<Option<ScanRun>>> = OnceLock::new();
    SCAN.get_or_init(|| Mutex::new(None))
}

/// The keys of the run in flight, or the finished one anybody is polling.
fn with_scan<T>(reader: impl FnOnce(Option<&ScanRun>) -> T) -> T {
    let slot = scan_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reader(slot.as_ref())
}

/// One page of the history's textual entries, newest first, under a cursor
/// of `(captured_at_ms, event_id)` — the same key the search pagination
/// follows, so a scan walks the same order the list shows.
struct ScanPage {
    entries: Vec<(i64, String)>,
    cursor: Option<(i64, i64)>,
}

fn read_scan_page_blocking(
    store: &StoreHandle,
    cursor: Option<(i64, i64)>,
) -> Result<ScanPage, String> {
    store
        .with_reader(|connection| -> Result<ScanPage, rusqlite::Error> {
            let mut statement = connection.prepare(
                "SELECT he.event_id, substr(CAST(rp.inline_payload AS TEXT), 1, ?2)
                 FROM history_event he
                 JOIN content c ON c.content_id = he.content_id
                 JOIN event_representation er
                   ON er.event_id = he.event_id AND er.ordinal = 0
                 JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
                 WHERE c.kind IN ('text', 'link', 'code', 'html')
                   AND rp.inline_payload IS NOT NULL
                   AND (?3 IS NULL OR (he.captured_at_ms, he.event_id) < (?3, ?4))
                 ORDER BY he.captured_at_ms DESC, he.event_id DESC
                 LIMIT ?1",
            )?;
            let (cursor_ms, cursor_id) = match cursor {
                Some((ms, id)) => (Some(ms), Some(id)),
                None => (None, None),
            };
            let rows = statement.query_map(
                rusqlite::params![
                    SCAN_BATCH as i64,
                    MAX_ENTRY_TEXT_CHARS as i64,
                    cursor_ms,
                    cursor_id
                ],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )?;
            let mut entries = Vec::new();
            for row in rows {
                entries.push(row?);
            }
            let next_cursor = match entries.last() {
                Some(last) => fetch_cursor(connection, last.0)?,
                None => None,
            };
            Ok(ScanPage {
                entries,
                cursor: next_cursor,
            })
        })
        .map_err(|_| "scan_history_unavailable".to_owned())
}

fn fetch_cursor(
    connection: &rusqlite::Connection,
    event_id: i64,
) -> rusqlite::Result<Option<(i64, i64)>> {
    let cursor = connection.query_row(
        "SELECT captured_at_ms, event_id FROM history_event WHERE event_id = ?1",
        [event_id],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    );
    match cursor {
        Ok(pair) => Ok(Some(pair)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Counts every entry the scan would look at, so the progress bar is a
/// number and not a feeling.
fn count_scannable_blocking(store: &StoreHandle) -> Result<usize, String> {
    store
        .with_reader(|connection| {
            connection.query_row(
                "SELECT COUNT(*)
                 FROM history_event he
                 JOIN content c ON c.content_id = he.content_id
                 JOIN event_representation er
                   ON er.event_id = he.event_id AND er.ordinal = 0
                 JOIN raw_payload rp ON rp.raw_payload_id = er.raw_payload_id
                 WHERE c.kind IN ('text', 'link', 'code', 'html')
                   AND rp.inline_payload IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            )
        })
        .map(|count| usize::try_from(count).unwrap_or(0))
        .map_err(|_| "scan_history_unavailable".to_owned())
}

// ------------------------------------------------------- wire, and pure --

/// Builds the request body for one batch: the entries as a list in the
/// state, one narrow question per entry, each pointing at its own entry by
/// the backticked path — the shape the multi-entry experiment ran on.
///
/// Pure on purpose: the body is the part worth testing, and a test should
/// not need a network to say it is well-formed.
pub fn scan_request_body(entries: &[(i64, String)]) -> serde_json::Value {
    let questions: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        .enumerate()
        .map(|(index, _)| {
            (
                format!("e{index}"),
                json!({
                    "type": "noul",
                    "instructions": format!(
                        "Would disclosing `entries[{index}]` to a third party risk the owner's privacy or security? Judge only the entry at `entries[{index}]`."
                    ),
                    "criteria": {
                        "true": "Credentials, keys, tokens, codes, government IDs, financial details, or contact data identifying the owner",
                        "false": "Ordinary content whose disclosure would expose nobody"
                    }
                }),
            )
        })
        .collect();
    json!({
        "state": { "entries": entries.iter().map(|(_, text)| text).collect::<Vec<_>>() },
        "model": "jev-latest",
        "questions": questions,
    })
}

/// Reads one batch answer back into probabilities, in entry order.
pub fn probabilities_from_response(body: &str, entries: usize) -> Result<Vec<f64>, &'static str> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "scan_answer_malformed")?;
    let answers = value
        .get("answers")
        .ok_or("scan_answer_malformed")?
        .as_object()
        .ok_or("scan_answer_malformed")?;
    let mut probabilities = Vec::with_capacity(entries);
    for index in 0..entries {
        let noul = answers
            .get(&format!("e{index}"))
            .and_then(|answer| answer.get("noul"))
            .and_then(|noul| noul.as_f64())
            .ok_or("scan_answer_malformed")?;
        if !(0.0..=1.0).contains(&noul) {
            return Err("scan_answer_malformed");
        }
        probabilities.push(noul);
    }
    Ok(probabilities)
}

async fn ask_typesafe(key: &str, body: &serde_json::Value) -> Result<String, &'static str> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| "scan_network_error")?;
    let request = client
        .post("https://api.typesafe.ai/v1/systemone")
        .bearer_auth(key)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_string(body).map_err(|_| "scan_answer_malformed")?)
        .build()
        .map_err(|_| "scan_network_error")?;
    let response = client
        .execute(request)
        .await
        .map_err(|_| "scan_network_error")?;
    match response.status().as_u16() {
        200 => Ok(response.text().await.map_err(|_| "scan_network_error")?),
        401 | 403 => Err("scan_key_refused"),
        429 => Err("scan_rate_limited"),
        _ => Err("scan_provider_refused"),
    }
}

// ------------------------------------------------------- scan lifecycle --

pub async fn typesafe_scan_start_service(state: &crate::state::AppState) -> Result<String, String> {
    let settings = {
        let store = state.store.clone();
        // Synchronous on purpose: start is a button press, and the settings
        // row is one indexed read.
        read_typesafe_settings_blocking(&store)?
    };
    if settings.api_key.is_empty() {
        return Err("scan_key_missing".to_owned());
    }
    let store = state.store.clone();
    let total = crate::commands::run_blocking("scan_history_unavailable", move || {
        count_scannable_blocking(&store)
    })
    .await?;

    // One scan at a time: a second press while one runs joins the running
    // one rather than racing it through the same history.
    {
        let slot = scan_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(run) = slot.as_ref()
            && run.progress.state == ScanState::Running
        {
            return Ok(run.progress.run_id.clone());
        }
    }

    let run_id = uuid::Uuid::new_v4().to_string();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut slot = scan_slot()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(ScanRun {
            progress: ScanProgressDto {
                run_id: run_id.clone(),
                state: ScanState::Running,
                processed: 0,
                total,
                flagged: Vec::new(),
                error_code: None,
            },
            cancel: Arc::clone(&cancel),
        });
    }

    let store = state.store.clone();
    let key = settings.api_key;
    tokio::spawn(async move {
        let mut cursor: Option<(i64, i64)> = None;
        loop {
            if cancel.load(Ordering::Relaxed) {
                finish(|progress| {
                    progress.state = ScanState::Completed;
                });
                return;
            }
            let page = {
                let store = store.clone();
                match crate::commands::run_blocking("scan_history_unavailable", move || {
                    read_scan_page_blocking(&store, cursor)
                })
                .await
                {
                    Ok(page) => page,
                    Err(code) => {
                        finish(|progress| {
                            progress.state = ScanState::Failed;
                            progress.error_code = Some(code);
                        });
                        return;
                    }
                }
            };
            if page.entries.is_empty() {
                finish(|progress| {
                    progress.state = ScanState::Completed;
                });
                return;
            }
            let count = page.entries.len();
            match ask_typesafe(&key, &scan_request_body(&page.entries)).await {
                Ok(body) => match probabilities_from_response(&body, count) {
                    Ok(probabilities) => {
                        let mut flagged = Vec::new();
                        for ((event_id, text), probability) in
                            page.entries.iter().zip(probabilities.iter())
                        {
                            if *probability >= FLAG_THRESHOLD {
                                flagged.push(FlaggedEntryDto {
                                    event_id: *event_id,
                                    // The preview shows enough to judge the
                                    // flag, never the whole entry.
                                    preview: text.chars().take(200).collect(),
                                    probability: *probability,
                                });
                            }
                        }
                        cursor = page.cursor;
                        update(|progress| {
                            progress.processed += count;
                            progress.flagged.extend(flagged);
                        });
                    }
                    Err(code) => {
                        finish(|progress| {
                            progress.state = ScanState::Failed;
                            progress.error_code = Some(code.to_owned());
                        });
                        return;
                    }
                },
                Err(code) => {
                    finish(|progress| {
                        progress.state = ScanState::Failed;
                        progress.error_code = Some(code.to_owned());
                    });
                    return;
                }
            }
            if cursor.is_none() {
                finish(|progress| {
                    progress.state = ScanState::Completed;
                });
                return;
            }
        }
    });

    Ok(run_id)
}

fn update(mutate: impl FnOnce(&mut ScanProgressDto)) {
    let mut slot = scan_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(run) = slot.as_mut() {
        mutate(&mut run.progress);
    }
}

fn finish(mutate: impl FnOnce(&mut ScanProgressDto)) {
    update(|progress| {
        mutate(progress);
    });
}

pub fn typesafe_scan_status_service() -> ScanProgressDto {
    with_scan(|run| {
        run.map(|run| run.progress.clone())
            .unwrap_or(ScanProgressDto {
                run_id: String::new(),
                state: ScanState::Completed,
                processed: 0,
                total: 0,
                flagged: Vec::new(),
                error_code: None,
            })
    })
}

pub fn typesafe_scan_stop_service() -> Result<bool, String> {
    let slot = scan_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match slot.as_ref() {
        Some(run) if run.progress.state == ScanState::Running => {
            run.cancel.store(true, Ordering::Relaxed);
            Ok(true)
        }
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FLAG_THRESHOLD, SCAN_BATCH, TypesafeSettingsDto, probabilities_from_response,
        scan_request_body, validate_typesafe_settings,
    };

    #[test]
    fn the_request_asks_one_narrow_question_per_entry() {
        let body = scan_request_body(&[
            (1, "haslo: Kropka12".to_owned()),
            (2, "mleko, chleb".to_owned()),
        ]);
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["state"]["entries"].as_array().unwrap().len(), 2);
        let questions = body["questions"].as_object().unwrap();
        assert_eq!(questions.len(), 2);
        assert!(questions.contains_key("e0"));
        assert!(questions.contains_key("e1"));
        // Each question judges exactly its own entry, by path.
        assert!(
            questions["e0"]["instructions"]
                .as_str()
                .unwrap()
                .contains("`entries[0]`")
        );
        // And no entry text leaks into the question itself.
        assert!(
            !questions["e0"]["instructions"]
                .as_str()
                .unwrap()
                .contains("Kropka")
        );
    }

    #[test]
    fn the_answer_comes_back_as_probabilities_in_order() {
        let body =
            r#"{"answers":{"e0":{"type":"noul","noul":0.97},"e1":{"type":"noul","noul":0.08}}}"#;
        assert_eq!(
            probabilities_from_response(body, 2).unwrap(),
            vec![0.97, 0.08]
        );
        // Missing entry, or a probability outside this world: refused as
        // malformed rather than quietly trusted.
        let missing = r#"{"answers":{"e0":{"type":"noul","noul":0.5}}}"#;
        assert!(probabilities_from_response(missing, 2).is_err());
        let absurd = r#"{"answers":{"e0":{"noul":1.7},"e1":{"noul":0.1}}}"#;
        assert!(probabilities_from_response(absurd, 2).is_err());
    }

    #[test]
    fn the_flag_threshold_sits_above_honest_unsureness() {
        // The experiment's weakest case — a bare IBAN, no context — came
        // back at 0.68 and must not be flagged; a password came at 0.98
        // and must be. The threshold lives between them; written as a
        // runtime comparison over the constants so clippy does not fold
        // it into a const assertion.
        let weakest_honest: f64 = 0.68;
        let strongest_flagged: f64 = 0.98;
        assert!(strongest_flagged >= FLAG_THRESHOLD);
        assert!(weakest_honest < FLAG_THRESHOLD);
    }

    #[test]
    fn batch_and_settings_bounds_are_the_contract() {
        assert_eq!(SCAN_BATCH, 20);
        let mut settings = TypesafeSettingsDto::default();
        assert!(validate_typesafe_settings(&settings).is_ok());
        settings.api_key = "ts_key".to_owned();
        assert!(validate_typesafe_settings(&settings).is_ok());
        settings.api_key = " padded ".to_owned();
        assert!(validate_typesafe_settings(&settings).is_err());
    }
}
