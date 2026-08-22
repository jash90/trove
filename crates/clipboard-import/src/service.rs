use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
    sync::{Arc, Mutex},
};

use clipboard_store::{
    BeginImportRun, ImportFailureCount, ImportSourceKind, ImportWorkerLease, ResumeImportRun,
    StoreError, StoreHandle, StoreImportCandidate, StoreImportRunState, StoreImportRunStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    FramedHasher, ImportCandidate, ImportError, ImportParseReport, ImportSource, detect_export,
    parse_export_report,
};

pub const IMPORT_BATCH_SIZE: usize = clipboard_store::IMPORT_BATCH_SIZE;
const PREPARED_SESSION_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportRunHandle {
    pub run_id: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportRunState {
    Running,
    Completed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportAnalysis {
    pub analysis_id: Uuid,
    pub total: u64,
    pub candidate_records: u64,
    pub failed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportSummary {
    pub run_id: Uuid,
    pub total: u64,
    pub imported: u64,
    pub already_present: u64,
    pub skipped: u64,
    pub failed: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub run_id: Uuid,
    pub state: ImportRunState,
    pub processed: u64,
    pub total: u64,
    pub imported: u64,
    pub already_present: u64,
    pub skipped: u64,
    pub failed: u64,
    pub error_code: Option<String>,
    pub summary: Option<ImportSummary>,
}

#[derive(Clone)]
pub struct ImportWorkerPolicy {
    interrupt_after_batches: Option<usize>,
    start_gate: Option<Arc<tokio::sync::Notify>>,
    completion_signal: Option<Arc<tokio::sync::Notify>>,
}

impl ImportWorkerPolicy {
    pub const fn unbounded() -> Self {
        Self {
            interrupt_after_batches: None,
            start_gate: None,
            completion_signal: None,
        }
    }

    #[doc(hidden)]
    pub fn interrupt_after_batches(batch_count: usize) -> Self {
        assert!(batch_count > 0, "batch interruption count must be positive");
        Self {
            interrupt_after_batches: Some(batch_count),
            start_gate: None,
            completion_signal: None,
        }
    }

    #[doc(hidden)]
    pub fn wait_before_work(
        start_gate: Arc<tokio::sync::Notify>,
        completion_signal: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            interrupt_after_batches: None,
            start_gate: Some(start_gate),
            completion_signal: Some(completion_signal),
        }
    }
}

impl Default for ImportWorkerPolicy {
    fn default() -> Self {
        Self::unbounded()
    }
}

#[derive(Clone)]
pub struct ImportService {
    store: StoreHandle,
    worker_policy: Arc<ImportWorkerPolicy>,
    prepared_sessions: Arc<Mutex<PreparedSessions>>,
}

impl ImportService {
    pub fn new(store: StoreHandle) -> Self {
        Self::with_worker_policy(store, ImportWorkerPolicy::default())
    }

    #[doc(hidden)]
    pub fn with_worker_policy(store: StoreHandle, worker_policy: ImportWorkerPolicy) -> Self {
        Self {
            store,
            worker_policy: Arc::new(worker_policy),
            prepared_sessions: Arc::new(Mutex::new(PreparedSessions::default())),
        }
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    pub fn analyze(&self, path: impl AsRef<Path>) -> Result<ImportAnalysis, ImportError> {
        let source = prepare_source(path.as_ref())?;
        let total = source.total_records;
        let candidate_records = source.candidate_records();
        let failed = source.initial_failed_records();
        let analysis_id = self
            .prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .insert(source);
        Ok(ImportAnalysis {
            analysis_id,
            total,
            candidate_records,
            failed,
        })
    }

    pub async fn begin(&self, analysis_id: Uuid) -> Result<ImportRunHandle, ImportError> {
        let source = self
            .prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .take(analysis_id)
            .ok_or_else(|| ImportError::service("analysis_not_found"))?;
        let status = self.persist_run(&source).await?;
        let run_id = status.run_id;
        let generation = status.generation;
        let worker = self.clone();
        tokio::spawn(async move {
            let _ = worker
                .run_worker(run_id, generation, source.candidates, 0)
                .await;
        });
        Ok(ImportRunHandle { run_id })
    }

    pub fn status(&self, run_id: Uuid) -> Result<ImportProgress, ImportError> {
        let status = self.store.import_status(run_id).map_err(map_store_error)?;
        progress_from_store(status)
    }

    pub async fn resume(
        &self,
        run_id: Uuid,
        path: impl AsRef<Path>,
    ) -> Result<ImportRunHandle, ImportError> {
        let source = prepare_source(path.as_ref())?;
        let lease = self
            .store
            .resume_import(ResumeImportRun {
                run_id,
                source_kind: source.source_kind,
                source_fingerprint: source.source_fingerprint,
                total_records: source.total_records,
                candidate_records: source.candidate_records(),
            })
            .await
            .map_err(map_store_error)?;
        let offset = usize::try_from(lease.next_candidate_offset)
            .map_err(|_| ImportError::service("invalid_run_state"))?;
        if offset > source.candidates.len() {
            return Err(ImportError::service("invalid_run_state"));
        }
        let worker = self.clone();
        tokio::spawn(async move {
            let _ = worker
                .run_worker(run_id, lease.generation, source.candidates, offset)
                .await;
        });
        Ok(ImportRunHandle { run_id })
    }

    pub async fn run_to_completion(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<ImportSummary, ImportError> {
        let source = prepare_source(path.as_ref())?;
        let status = self.persist_run(&source).await?;
        match self
            .run_worker(status.run_id, status.generation, source.candidates, 0)
            .await?
        {
            WorkerCompletion::Completed => self
                .status(status.run_id)?
                .summary
                .ok_or_else(|| ImportError::service("invalid_run_state")),
            WorkerCompletion::Interrupted => Err(ImportError::service("worker_interrupted")),
            WorkerCompletion::Superseded => Err(ImportError::service("worker_superseded")),
        }
    }

    async fn persist_run(&self, source: &PreparedSource) -> Result<ImportWorkerLease, ImportError> {
        self.store
            .begin_import(BeginImportRun {
                source_kind: source.source_kind,
                source_fingerprint: source.source_fingerprint,
                total_records: source.total_records,
                candidate_records: source.candidate_records(),
                initial_failures: source.failure_counts.clone(),
            })
            .await
            .map_err(map_store_error)
    }

    async fn run_worker(
        &self,
        run_id: Uuid,
        generation: u64,
        candidates: Vec<ImportCandidate>,
        offset: usize,
    ) -> Result<WorkerCompletion, ImportError> {
        let _completion_signal =
            WorkerCompletionSignal(self.worker_policy.completion_signal.clone());
        if let Some(start_gate) = &self.worker_policy.start_gate {
            start_gate.notified().await;
        }
        let mut remaining = candidates
            .into_iter()
            .skip(offset)
            .enumerate()
            .map(|(relative_offset, candidate)| {
                store_candidate((offset + relative_offset) as u64, candidate)
            })
            .peekable();
        let mut completed_batches = 0_usize;
        while remaining.peek().is_some() {
            let batch = remaining
                .by_ref()
                .take(IMPORT_BATCH_SIZE)
                .collect::<Vec<_>>();
            let expected = batch.len() as u64;
            let outcome = match self.store.import_batch(run_id, generation, batch).await {
                Ok(outcome) => outcome,
                Err(StoreError::ImportWorkerSuperseded) => {
                    return Ok(WorkerCompletion::Superseded);
                }
                Err(_) => {
                    let _ = self
                        .store
                        .fail_import(run_id, generation, "store_failure")
                        .await;
                    return Err(ImportError::service("store_failure"));
                }
            };
            if outcome.processed_candidates != expected {
                let _ = self
                    .store
                    .fail_import(run_id, generation, "checkpoint_failure")
                    .await;
                return Err(ImportError::service("checkpoint_failure"));
            }
            completed_batches += 1;
            if remaining.peek().is_some()
                && self
                    .worker_policy
                    .interrupt_after_batches
                    .is_some_and(|limit| completed_batches >= limit)
            {
                return Ok(WorkerCompletion::Interrupted);
            }
            tokio::task::yield_now().await;
        }
        match self.store.finish_import(run_id, generation).await {
            Ok(_) => {}
            Err(StoreError::ImportWorkerSuperseded) => {
                return Ok(WorkerCompletion::Superseded);
            }
            Err(_) => {
                let _ = self
                    .store
                    .fail_import(run_id, generation, "finalization_failure")
                    .await;
                return Err(ImportError::service("finalization_failure"));
            }
        }
        Ok(WorkerCompletion::Completed)
    }
}

#[derive(Default)]
struct PreparedSessions {
    sources: BTreeMap<Uuid, PreparedSource>,
    insertion_order: VecDeque<Uuid>,
}

impl PreparedSessions {
    fn insert(&mut self, source: PreparedSource) -> Uuid {
        while self.sources.len() >= PREPARED_SESSION_CAPACITY {
            if let Some(expired) = self.insertion_order.pop_front() {
                self.sources.remove(&expired);
            }
        }
        let analysis_id = Uuid::now_v7();
        self.sources.insert(analysis_id, source);
        self.insertion_order.push_back(analysis_id);
        analysis_id
    }

    fn take(&mut self, analysis_id: Uuid) -> Option<PreparedSource> {
        let source = self.sources.remove(&analysis_id)?;
        self.insertion_order
            .retain(|candidate| *candidate != analysis_id);
        Some(source)
    }
}

enum WorkerCompletion {
    Completed,
    Interrupted,
    Superseded,
}

struct WorkerCompletionSignal(Option<Arc<tokio::sync::Notify>>);

impl Drop for WorkerCompletionSignal {
    fn drop(&mut self) {
        if let Some(signal) = &self.0 {
            signal.notify_one();
        }
    }
}

struct PreparedSource {
    source_kind: ImportSourceKind,
    source_fingerprint: [u8; 32],
    total_records: u64,
    candidates: Vec<ImportCandidate>,
    failure_counts: Vec<ImportFailureCount>,
}

impl PreparedSource {
    fn candidate_records(&self) -> u64 {
        self.candidates.len() as u64
    }

    fn initial_failed_records(&self) -> u64 {
        self.failure_counts
            .iter()
            .map(|failure| failure.count)
            .sum()
    }
}

fn prepare_source(path: &Path) -> Result<PreparedSource, ImportError> {
    prepare_source_with_hook(path, || {})
}

fn prepare_source_with_hook(
    path: &Path,
    after_initial_detection: impl FnOnce(),
) -> Result<PreparedSource, ImportError> {
    let detected = detect_export(path)?;
    after_initial_detection();
    let report = parse_export_report(path);
    let verified = detect_export(path);
    let source_is_unchanged = verified.as_ref().is_ok_and(|verified| {
        verified.source == detected.source
            && verified.source_fingerprint == detected.source_fingerprint
    });
    if !source_is_unchanged {
        return Err(ImportError::service("source_changed"));
    }
    let report = report?;
    let total_records =
        u64::try_from(report.total).map_err(|_| ImportError::service("source_too_large"))?;
    let source_fingerprint = prepared_snapshot_fingerprint(
        detected.source,
        detected.source_fingerprint,
        total_records,
        &report,
    )?;
    let failure_counts = aggregate_failures(&report);
    Ok(PreparedSource {
        source_kind: store_source(detected.source),
        source_fingerprint,
        total_records,
        candidates: report.candidates,
        failure_counts,
    })
}

fn prepared_snapshot_fingerprint(
    source: ImportSource,
    manifest_fingerprint: [u8; 32],
    total_records: u64,
    report: &ImportParseReport,
) -> Result<[u8; 32], ImportError> {
    let mut hasher = FramedHasher::new();
    hasher.add_optional(Some(b"clipboard-import.prepared-snapshot-v1"));
    hasher.add_optional(Some(source.as_str().as_bytes()));
    hasher.add_optional(Some(&manifest_fingerprint));
    hasher.add_optional(Some(&total_records.to_be_bytes()));
    hasher.add_optional(Some(&(report.candidates.len() as u64).to_be_bytes()));
    for candidate in &report.candidates {
        hasher.add_optional(Some(b"candidate"));
        hasher.add_optional(Some(&candidate.record_fingerprint));
    }
    hasher.add_optional(Some(&(report.failures.len() as u64).to_be_bytes()));
    for failure in &report.failures {
        let record =
            u64::try_from(failure.record).map_err(|_| ImportError::service("source_too_large"))?;
        hasher.add_optional(Some(b"failure"));
        hasher.add_optional(Some(&record.to_be_bytes()));
        hasher.add_optional(Some(failure.reason.as_bytes()));
    }
    Ok(hasher.finish())
}

fn aggregate_failures(report: &ImportParseReport) -> Vec<ImportFailureCount> {
    let mut counts = BTreeMap::<&'static str, u64>::new();
    for failure in &report.failures {
        *counts.entry(failure.reason).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(reason_code, count)| ImportFailureCount {
            reason_code: reason_code.to_owned(),
            count,
        })
        .collect()
}

fn store_source(source: ImportSource) -> ImportSourceKind {
    match source {
        ImportSource::Raycast => ImportSourceKind::Raycast,
        ImportSource::SuperCmd => ImportSourceKind::SuperCmd,
    }
}

fn store_candidate(candidate_offset: u64, candidate: ImportCandidate) -> StoreImportCandidate {
    let search_text = combined_search_text(
        candidate.primary_text.as_deref(),
        candidate.search_ocr.as_deref(),
    );
    StoreImportCandidate {
        candidate_offset,
        record_fingerprint: candidate.record_fingerprint,
        capture: candidate.capture,
        search_text,
        source_app_original: candidate.source_application_path,
    }
}

fn combined_search_text(primary_text: Option<&str>, search_ocr: Option<&str>) -> Option<String> {
    match (primary_text, search_ocr) {
        (Some(primary), Some(ocr)) => Some(format!("{primary}\n{ocr}")),
        (Some(primary), None) => Some(primary.to_owned()),
        (None, Some(ocr)) => Some(ocr.to_owned()),
        (None, None) => None,
    }
}

fn progress_from_store(status: StoreImportRunStatus) -> Result<ImportProgress, ImportError> {
    let processed = status
        .imported_records
        .checked_add(status.already_present_records)
        .and_then(|count| count.checked_add(status.skipped_records))
        .and_then(|count| count.checked_add(status.failed_records))
        .ok_or_else(|| ImportError::service("invalid_run_state"))?;
    if processed > status.total_records {
        return Err(ImportError::service("invalid_run_state"));
    }
    let state = match status.state {
        StoreImportRunState::Running => ImportRunState::Running,
        StoreImportRunState::Completed => ImportRunState::Completed,
        StoreImportRunState::Failed => ImportRunState::Failed,
    };
    if state == ImportRunState::Completed && processed != status.total_records {
        return Err(ImportError::service("invalid_run_state"));
    }
    let summary = (state == ImportRunState::Completed).then_some(ImportSummary {
        run_id: status.run_id,
        total: status.total_records,
        imported: status.imported_records,
        already_present: status.already_present_records,
        skipped: status.skipped_records,
        failed: status.failed_records,
    });
    Ok(ImportProgress {
        run_id: status.run_id,
        state,
        processed,
        total: status.total_records,
        imported: status.imported_records,
        already_present: status.already_present_records,
        skipped: status.skipped_records,
        failed: status.failed_records,
        error_code: status.error_code,
        summary,
    })
}

fn map_store_error(error: StoreError) -> ImportError {
    let reason = match error {
        StoreError::ImportRunNotFound => "run_not_found",
        StoreError::ImportSourceMismatch => "source_mismatch",
        StoreError::ImportRunNotResumable => "run_not_resumable",
        StoreError::ImportCheckpointMismatch => "checkpoint_failure",
        StoreError::ImportWorkerSuperseded => "worker_superseded",
        StoreError::ImportInvariant => "invalid_run_state",
        StoreError::InvalidImportInput | StoreError::ImportBatchTooLarge => "invalid_import",
        _ => "store_failure",
    };
    ImportError::service(reason)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn source_changed_during_analysis_is_rejected_before_a_run_can_be_persisted() {
        let export = tempfile::tempdir().unwrap();
        let manifest = export.path().join("clipboard.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&[json!({
                "createdAt": "2026-08-22T12:00:00Z",
                "modifiedAt": "2026-08-22T12:00:00Z",
                "category": "text",
                "copyCount": 1,
                "text": "synthetic before",
            })])
            .unwrap(),
        )
        .unwrap();

        let result = prepare_source_with_hook(export.path(), || {
            fs::write(
                &manifest,
                serde_json::to_vec(&[json!({
                    "createdAt": "2026-08-22T12:00:00Z",
                    "modifiedAt": "2026-08-22T12:00:00Z",
                    "category": "text",
                    "copyCount": 1,
                    "text": "synthetic after",
                })])
                .unwrap(),
            )
            .unwrap();
        });

        assert!(matches!(
            result,
            Err(ImportError::Service {
                reason: "source_changed"
            })
        ));
    }

    #[test]
    fn prepared_session_cache_evicts_the_oldest_unconsumed_analysis() {
        let mut sessions = PreparedSessions::default();
        let mut first = None;
        let mut newest = None;
        for index in 0..=PREPARED_SESSION_CAPACITY {
            let fingerprint_byte = u8::try_from(index).unwrap();
            let analysis_id = sessions.insert(PreparedSource {
                source_kind: ImportSourceKind::Raycast,
                source_fingerprint: [fingerprint_byte; 32],
                total_records: 0,
                candidates: Vec::new(),
                failure_counts: Vec::new(),
            });
            first.get_or_insert(analysis_id);
            newest = Some(analysis_id);
        }

        assert!(sessions.take(first.unwrap()).is_none());
        assert!(sessions.take(newest.unwrap()).is_some());
        assert_eq!(sessions.sources.len(), PREPARED_SESSION_CAPACITY - 1);
    }
}
