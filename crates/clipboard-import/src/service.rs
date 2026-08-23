use std::{
    collections::{BTreeMap, VecDeque},
    mem::size_of,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use clipboard_store::{
    BeginImportRun, ImportFailureCount, ImportOperationGate, ImportSourceKind, ImportWorkerLease,
    ResumeImportRun, StoreError, StoreHandle, StoreImportCandidate, StoreImportRunState,
    StoreImportRunStatus,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    FramedHasher, ImportCandidate, ImportError, ImportParseLimits, ImportParseReport, ImportSource,
    detect::{bounded_path_copy, detect_export_with_permit},
    parse_detected_export_report_with_permit,
};

pub const IMPORT_BATCH_SIZE: usize = clipboard_store::IMPORT_BATCH_SIZE;
pub const MAX_IMPORT_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMPORT_RECORD_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IMPORT_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_IMPORT_BATCH_BYTES: usize = clipboard_store::MAX_IMPORT_BATCH_BYTES;
pub const MAX_IMPORT_AUXILIARY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PREPARED_SOURCE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_PREPARED_CACHE_BYTES: usize = 192 * 1024 * 1024;
pub const MAX_IMPORT_OPERATION_BYTES: usize = clipboard_store::MAX_IMPORT_OPERATION_BYTES;
pub const MAX_IMPORT_RUNTIME_BYTES: usize = 256 * 1024 * 1024;
pub const PREPARED_SESSION_CAPACITY: usize = 32;
pub const PREPARED_SESSION_TTL: Duration = Duration::from_secs(15 * 60);
const PREPARED_SESSION_OVERHEAD_BYTES: usize = 16 * 1024;
const MAX_PREPARED_SESSION_CONTROL_BYTES: usize = 16 * 1024;
const MAX_PREPARED_FAILURE_CONTROL_BYTES: usize = 16 * 1024;
const MAX_IMPORT_HANDOFF_CONTROL_BYTES: usize = MAX_PREPARED_FAILURE_CONTROL_BYTES;
const MAX_IMPORT_FAILURE_KINDS: usize = 8;
pub const MAX_PREPARED_SOURCE_CONTROL_BYTES: usize = 2 * 1024 * 1024;
const MAX_PREPARED_RUNTIME_CONTROL_BYTES: usize = 64 * 1024;

const _: () = assert!(
    crate::detect::MAX_IMPORT_PATH_CONTROL_BYTES
        + crate::supercmd::MAX_AUXILIARY_TRAVERSAL_CONTROL_BYTES
        + MAX_PREPARED_FAILURE_CONTROL_BYTES
        + crate::MAX_IMPORT_ANALYSIS_CONTROL_BYTES
        + PREPARED_SESSION_OVERHEAD_BYTES
        <= MAX_PREPARED_SOURCE_CONTROL_BYTES
);
const _: () = assert!(
    MAX_PREPARED_SESSION_CONTROL_BYTES + MAX_IMPORT_HANDOFF_CONTROL_BYTES
        <= MAX_PREPARED_RUNTIME_CONTROL_BYTES
);

/// Hard admission limits shared by analyzed sessions, resume preparations, CLI imports, and
/// active workers. Production reserves one full per-source allowance before every parse so the
/// number and aggregate memory envelope remain finite even while parsing.
#[derive(Clone, Copy)]
pub(crate) struct ImportAdmissionLimits {
    max_sources: usize,
    max_source_bytes: usize,
    source_control_bytes: usize,
    prepared_cache_bytes: usize,
    runtime_control_bytes: usize,
    operation_bytes: usize,
    runtime_bytes: usize,
}

impl ImportAdmissionLimits {
    #[doc(hidden)]
    pub(crate) const fn new(
        max_sources: usize,
        max_source_bytes: usize,
        source_control_bytes: usize,
        prepared_cache_bytes: usize,
        runtime_control_bytes: usize,
        operation_bytes: usize,
        runtime_bytes: usize,
    ) -> Self {
        assert!(max_sources > 0, "source admission count must be positive");
        assert!(
            source_control_bytes > 0 && source_control_bytes < max_source_bytes,
            "source control must be a strict carve-out of one source slot"
        );
        assert!(
            runtime_control_bytes >= MAX_PREPARED_SESSION_CONTROL_BYTES,
            "runtime control must fit the bounded session collection"
        );
        assert!(
            matches!(runtime_control_bytes.checked_add(max_source_bytes), Some(required) if prepared_cache_bytes >= required),
            "aggregate admission size must fit fixed control and one source"
        );
        assert!(operation_bytes > 0, "operation admission must be positive");
        assert!(
            matches!(prepared_cache_bytes.checked_add(operation_bytes), Some(total) if total <= runtime_bytes),
            "cache and operation admissions must fit the runtime envelope"
        );
        Self {
            max_sources,
            max_source_bytes,
            source_control_bytes,
            prepared_cache_bytes,
            runtime_control_bytes,
            operation_bytes,
            runtime_bytes,
        }
    }
}

impl Default for ImportAdmissionLimits {
    fn default() -> Self {
        Self::new(
            PREPARED_SESSION_CAPACITY,
            MAX_PREPARED_SOURCE_BYTES,
            MAX_PREPARED_SOURCE_CONTROL_BYTES,
            MAX_PREPARED_CACHE_BYTES,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_RUNTIME_BYTES,
        )
    }
}

#[derive(Clone)]
pub(crate) struct ImportRuntime {
    admission_budget: AdmissionBudget,
    operation_gate: ImportOperationGate,
    prepared_sessions: Arc<Mutex<PreparedSessions>>,
    limits: ImportAdmissionLimits,
}

impl ImportRuntime {
    pub(crate) fn process_wide() -> Self {
        static RUNTIME: OnceLock<ImportRuntime> = OnceLock::new();
        RUNTIME
            .get_or_init(|| Self::build(ImportAdmissionLimits::default()))
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn with_limits(limits: ImportAdmissionLimits) -> Self {
        assert_eq!(limits.operation_bytes, MAX_IMPORT_OPERATION_BYTES);
        Self::build(limits)
    }

    fn build(limits: ImportAdmissionLimits) -> Self {
        // The ledger carries the fixed runtime charge before the session collection or operation
        // gate is constructed, so those allocations never exist outside an admitted baseline.
        let admission_budget = AdmissionBudget::new(limits);
        let prepared_sessions = PreparedSessions::with_control(limits.runtime_control_bytes);
        let operation_gate = ImportOperationGate::process_wide();
        Self {
            admission_budget,
            operation_gate,
            prepared_sessions: Arc::new(Mutex::new(prepared_sessions)),
            limits,
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> (usize, usize) {
        let retained_bytes = self
            .admission_budget
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retained_bytes;
        (retained_bytes, self.operation_gate.active_bytes())
    }

    pub(crate) fn acquire_operation_blocking(
        &self,
    ) -> Result<clipboard_store::ImportOperationPermit, ImportError> {
        debug_assert_eq!(self.operation_gate.capacity(), self.limits.operation_bytes);
        debug_assert!(
            self.limits
                .prepared_cache_bytes
                .checked_add(self.limits.operation_bytes)
                .is_some_and(|bytes| bytes <= self.limits.runtime_bytes)
        );
        self.operation_gate
            .acquire_blocking()
            .map_err(|_| ImportError::service("analysis_unavailable"))
    }

    pub(crate) fn reserve_preparation(&self) -> Result<AdmissionReservation, ImportError> {
        self.prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .evict_expired(Instant::now());
        loop {
            if let Some(reservation) = self.admission_budget.try_reserve()? {
                return Ok(reservation);
            }
            let evicted = self
                .prepared_sessions
                .lock()
                .map_err(|_| ImportError::service("analysis_unavailable"))?
                .evict_oldest_available();
            if !evicted {
                return Err(ImportError::service("analysis_capacity_full"));
            }
        }
    }

    fn parse_limits(&self) -> ImportParseLimits {
        let json_record_bytes = self
            .limits
            .operation_bytes
            .saturating_sub(crate::JSON_READER_BUFFER_BYTES)
            .clamp(1, MAX_IMPORT_RECORD_BYTES);
        let csv_fixed_bytes =
            crate::CSV_INPUT_BUFFER_BYTES.saturating_add(crate::CSV_DRAIN_BUFFER_BYTES);
        let csv_available = self.limits.operation_bytes.saturating_sub(csv_fixed_bytes);
        let header_bytes = MAX_IMPORT_HEADER_BYTES
            .min(csv_available / (size_of::<usize>() + 1))
            .max(1);
        ImportParseLimits {
            manifest_bytes: MAX_IMPORT_MANIFEST_BYTES.min(
                self.limits
                    .max_source_bytes
                    .saturating_sub(self.limits.source_control_bytes),
            ),
            record_bytes: json_record_bytes,
            header_bytes,
            source_bytes: self
                .limits
                .max_source_bytes
                .saturating_sub(self.limits.source_control_bytes),
            source_control_bytes: self.limits.source_control_bytes,
            auxiliary_bytes: MAX_IMPORT_AUXILIARY_BYTES.min(
                self.limits
                    .max_source_bytes
                    .saturating_sub(self.limits.source_control_bytes),
            ),
        }
    }
}

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
    persistence_started: Option<Arc<tokio::sync::Notify>>,
    persistence_gate: Option<Arc<tokio::sync::Notify>>,
    fail_next_persistence: Arc<AtomicBool>,
    invalid_lease_offset_once: Arc<AtomicBool>,
    #[cfg(test)]
    force_checkpoint_mismatch_once: Arc<AtomicBool>,
    #[cfg(test)]
    failure_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl ImportWorkerPolicy {
    pub fn unbounded() -> Self {
        Self {
            interrupt_after_batches: None,
            start_gate: None,
            completion_signal: None,
            persistence_started: None,
            persistence_gate: None,
            fail_next_persistence: Arc::new(AtomicBool::new(false)),
            invalid_lease_offset_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            force_checkpoint_mismatch_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            failure_hook: None,
        }
    }

    #[doc(hidden)]
    pub fn interrupt_after_batches(batch_count: usize) -> Self {
        assert!(batch_count > 0, "batch interruption count must be positive");
        Self {
            interrupt_after_batches: Some(batch_count),
            start_gate: None,
            completion_signal: None,
            persistence_started: None,
            persistence_gate: None,
            fail_next_persistence: Arc::new(AtomicBool::new(false)),
            invalid_lease_offset_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            force_checkpoint_mismatch_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            failure_hook: None,
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
            persistence_started: None,
            persistence_gate: None,
            fail_next_persistence: Arc::new(AtomicBool::new(false)),
            invalid_lease_offset_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            force_checkpoint_mismatch_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            failure_hook: None,
        }
    }

    #[doc(hidden)]
    pub fn wait_before_persistence(
        persistence_started: Arc<tokio::sync::Notify>,
        persistence_gate: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            interrupt_after_batches: None,
            start_gate: None,
            completion_signal: None,
            persistence_started: Some(persistence_started),
            persistence_gate: Some(persistence_gate),
            fail_next_persistence: Arc::new(AtomicBool::new(false)),
            invalid_lease_offset_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            force_checkpoint_mismatch_once: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            failure_hook: None,
        }
    }

    #[doc(hidden)]
    pub fn fail_next_persistence() -> Self {
        Self {
            fail_next_persistence: Arc::new(AtomicBool::new(true)),
            ..Self::unbounded()
        }
    }

    #[doc(hidden)]
    pub fn invalid_lease_offset_once() -> Self {
        Self {
            invalid_lease_offset_once: Arc::new(AtomicBool::new(true)),
            ..Self::unbounded()
        }
    }

    #[cfg(test)]
    pub fn fail_marking_with_hook(failure_hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            failure_hook: Some(failure_hook),
            ..Self::unbounded()
        }
    }

    #[cfg(test)]
    pub fn checkpoint_mismatch_with_hook(failure_hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self {
            force_checkpoint_mismatch_once: Arc::new(AtomicBool::new(true)),
            failure_hook: Some(failure_hook),
            ..Self::unbounded()
        }
    }

    #[cfg(test)]
    fn before_failure(&self) {
        if let Some(hook) = &self.failure_hook {
            hook();
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
    runtime: ImportRuntime,
    owner: ServiceOwnerToken,
    owner_lifetime: Arc<()>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ServiceOwnerToken(Uuid);

impl ImportService {
    pub fn new(store: StoreHandle) -> Self {
        Self::with_runtime_and_policy(
            store,
            ImportRuntime::process_wide(),
            ImportWorkerPolicy::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_runtime(store: StoreHandle, runtime: ImportRuntime) -> Self {
        Self::with_runtime_and_policy(store, runtime, ImportWorkerPolicy::default())
    }

    #[doc(hidden)]
    pub fn with_worker_policy(store: StoreHandle, worker_policy: ImportWorkerPolicy) -> Self {
        Self::with_runtime_and_policy(store, ImportRuntime::process_wide(), worker_policy)
    }

    #[cfg(test)]
    pub(crate) fn with_worker_policy_and_limits(
        store: StoreHandle,
        worker_policy: ImportWorkerPolicy,
        admission_limits: ImportAdmissionLimits,
    ) -> Self {
        Self::with_runtime_and_policy(
            store,
            ImportRuntime::with_limits(admission_limits),
            worker_policy,
        )
    }

    fn with_runtime_and_policy(
        store: StoreHandle,
        runtime: ImportRuntime,
        worker_policy: ImportWorkerPolicy,
    ) -> Self {
        Self {
            store,
            worker_policy: Arc::new(worker_policy),
            runtime,
            owner: ServiceOwnerToken(Uuid::now_v7()),
            owner_lifetime: Arc::new(()),
        }
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    pub fn analyze(&self, path: impl AsRef<Path>) -> Result<ImportAnalysis, ImportError> {
        let source = self.prepare(path.as_ref())?;
        let total = source.source.total_records;
        let candidate_records = source.source.candidate_records();
        let failed = source.source.initial_failed_records();
        let analysis_id = Uuid::now_v7();
        let analysis_id = self
            .runtime
            .prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .insert(self.owner, analysis_id, source, Instant::now());
        Ok(ImportAnalysis {
            analysis_id,
            total,
            candidate_records,
            failed,
        })
    }

    pub async fn begin(&self, analysis_id: Uuid) -> Result<ImportRunHandle, ImportError> {
        let action = self
            .runtime
            .prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .begin(self.owner, analysis_id, Instant::now());
        let completion = match action {
            BeginPreparedSession::Start { source, completion } => {
                let handoff = self.clone();
                let handoff_completion = completion.clone();
                tokio::spawn(async move {
                    handoff
                        .persist_and_start(analysis_id, source, handoff_completion)
                        .await;
                });
                completion
            }
            BeginPreparedSession::Wait(completion) => completion,
            BeginPreparedSession::Ready(handle) => return Ok(handle),
            BeginPreparedSession::Missing => {
                return Err(ImportError::service("analysis_not_found"));
            }
        };
        completion.wait().await
    }

    pub fn discard_analysis(&self, analysis_id: Uuid) -> Result<(), ImportError> {
        self.runtime
            .prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .discard(self.owner, analysis_id, Instant::now())
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
        let source = self.prepare_async(path.as_ref()).await?;
        let lease = self
            .store
            .resume_import(ResumeImportRun {
                run_id,
                source_kind: source.source.source_kind,
                source_fingerprint: source.source.source_fingerprint,
                total_records: source.source.total_records,
                candidate_records: source.source.candidate_records(),
            })
            .await
            .map_err(map_store_error)?;
        let offset = usize::try_from(lease.next_candidate_offset)
            .map_err(|_| ImportError::service("invalid_run_state"))?;
        if offset > source.source.candidates.len() {
            return Err(ImportError::service("invalid_run_state"));
        }
        let worker = self.clone();
        tokio::spawn(async move {
            let _ = worker
                .run_worker(run_id, lease.generation, source, offset)
                .await;
        });
        Ok(ImportRunHandle { run_id })
    }

    pub async fn run_to_completion(
        &self,
        path: impl AsRef<Path>,
    ) -> Result<ImportSummary, ImportError> {
        let source = self.prepare_async(path.as_ref()).await?;
        let status = self.persist_run(Uuid::now_v7(), &source.source).await?;
        match self
            .run_worker(status.run_id, status.generation, source, 0)
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

    async fn persist_run(
        &self,
        run_id: Uuid,
        source: &PreparedSource,
    ) -> Result<ImportWorkerLease, ImportError> {
        self.store
            .begin_import(BeginImportRun {
                run_id,
                source_kind: source.source_kind,
                source_fingerprint: source.source_fingerprint,
                total_records: source.total_records,
                candidate_records: source.candidate_records(),
                initial_failures: source.failure_counts.clone(),
            })
            .await
            .map_err(map_store_error)
    }

    fn prepare(&self, path: &Path) -> Result<PreparedSourceEnvelope, ImportError> {
        let operation_permit = self.runtime.acquire_operation_blocking()?;
        let reservation = self.runtime.reserve_preparation()?;
        self.prepare_with_admission(path, operation_permit, reservation)
    }

    async fn prepare_async(&self, path: &Path) -> Result<PreparedSourceEnvelope, ImportError> {
        let runtime = self.runtime.clone();
        let (operation_permit, reservation) = tokio::task::spawn_blocking(move || {
            let operation_permit = runtime.acquire_operation_blocking()?;
            let reservation = runtime.reserve_preparation()?;
            Ok::<_, ImportError>((operation_permit, reservation))
        })
        .await
        .map_err(|_| ImportError::service("analysis_unavailable"))??;
        let limits = self.runtime.parse_limits();
        let path = bounded_path_copy(path, limits)?;
        let preparer = self.clone();
        tokio::task::spawn_blocking(move || {
            preparer.prepare_with_admission(&path, operation_permit, reservation)
        })
        .await
        .map_err(|_| ImportError::service("analysis_unavailable"))?
    }

    fn prepare_with_admission(
        &self,
        path: &Path,
        operation_permit: clipboard_store::ImportOperationPermit,
        mut reservation: AdmissionReservation,
    ) -> Result<PreparedSourceEnvelope, ImportError> {
        let limits = self.runtime.parse_limits();
        let source = prepare_source_with_permit(path, &operation_permit, limits)?;
        reservation.resize(source.retained_bytes())?;
        Ok(PreparedSourceEnvelope {
            source,
            _reservation: reservation,
        })
    }

    async fn persist_and_start(
        &self,
        analysis_id: Uuid,
        source: PreparedSourceEnvelope,
        completion: Arc<PersistenceCompletion>,
    ) {
        if let Some(started) = &self.worker_policy.persistence_started {
            started.notify_one();
        }
        if let Some(gate) = &self.worker_policy.persistence_gate {
            gate.notified().await;
        }
        let persisted = if self
            .worker_policy
            .fail_next_persistence
            .swap(false, Ordering::AcqRel)
        {
            self.store
                .inject_begin_import_failure()
                .await
                .map_err(map_store_error)
        } else {
            self.persist_run(analysis_id, &source.source).await
        };
        match persisted {
            Ok(mut lease) => {
                if self
                    .worker_policy
                    .invalid_lease_offset_once
                    .swap(false, Ordering::AcqRel)
                {
                    lease.next_candidate_offset = u64::MAX;
                }
                let handle = ImportRunHandle {
                    run_id: analysis_id,
                };
                if lease.state == StoreImportRunState::Running {
                    let offset = match usize::try_from(lease.next_candidate_offset) {
                        Ok(offset) if offset <= source.source.candidates.len() => offset,
                        _ => {
                            if let Ok(mut sessions) = self.runtime.prepared_sessions.lock() {
                                sessions.restore(self.owner, analysis_id, source, Instant::now());
                            }
                            completion.finish(Err("invalid_run_state"));
                            return;
                        }
                    };
                    let marked_started = self
                        .runtime
                        .prepared_sessions
                        .lock()
                        .map(|mut sessions| sessions.mark_started(self.owner, analysis_id));
                    if !matches!(marked_started, Ok(true)) {
                        if let Ok(mut sessions) = self.runtime.prepared_sessions.lock() {
                            sessions.restore(self.owner, analysis_id, source, Instant::now());
                        }
                        completion.finish(Err("analysis_unavailable"));
                        return;
                    }
                    let worker = self.clone();
                    tokio::spawn(async move {
                        let completed = matches!(
                            worker
                                .run_worker(analysis_id, lease.generation, source, offset)
                                .await,
                            Ok(WorkerCompletion::Completed)
                        );
                        if let Ok(mut sessions) = worker.runtime.prepared_sessions.lock() {
                            sessions.finish_started(
                                worker.owner,
                                analysis_id,
                                completed,
                                Instant::now(),
                            );
                        }
                    });
                } else if let Ok(mut sessions) = self.runtime.prepared_sessions.lock() {
                    sessions.finish_completed(self.owner, analysis_id, handle, Instant::now());
                }
                completion.finish(Ok(handle));
            }
            Err(error) => {
                let reason = import_error_reason(&error);
                if let Ok(mut sessions) = self.runtime.prepared_sessions.lock() {
                    sessions.restore(self.owner, analysis_id, source, Instant::now());
                }
                completion.finish(Err(reason));
            }
        }
    }

    async fn run_worker(
        &self,
        run_id: Uuid,
        generation: u64,
        source: PreparedSourceEnvelope,
        offset: usize,
    ) -> Result<WorkerCompletion, ImportError> {
        let PreparedSourceEnvelope {
            source,
            _reservation,
        } = source;
        let _completion_signal =
            WorkerCompletionSignal(self.worker_policy.completion_signal.clone());
        if let Some(start_gate) = &self.worker_policy.start_gate {
            start_gate.notified().await;
        }
        let mut remaining = source.candidates.into_iter().skip(offset).enumerate().map(
            |(relative_offset, candidate)| {
                store_candidate((offset + relative_offset) as u64, candidate)
            },
        );
        let mut pending = None;
        let mut completed_batches = 0_usize;
        loop {
            if pending.is_none() && remaining.size_hint().0 == 0 {
                break;
            }
            let operation_gate = self.runtime.operation_gate.clone();
            let operation_permit =
                tokio::task::spawn_blocking(move || operation_gate.acquire_blocking())
                    .await
                    .map_err(|_| ImportError::service("analysis_unavailable"))?
                    .map_err(|_| ImportError::service("analysis_unavailable"))?;
            let batch_slots_bytes = IMPORT_BATCH_SIZE
                .checked_mul(size_of::<StoreImportCandidate>())
                .ok_or_else(|| ImportError::service("batch_too_large"))?;
            if batch_slots_bytes > MAX_IMPORT_BATCH_BYTES {
                return Err(ImportError::service("batch_too_large"));
            }
            let mut batch = Vec::with_capacity(IMPORT_BATCH_SIZE);
            let mut batch_bytes = batch_slots_bytes;
            while batch.len() < IMPORT_BATCH_SIZE {
                let Some(candidate) = pending.take().or_else(|| remaining.next()) else {
                    break;
                };
                if candidate.capture.representations.len()
                    > clipboard_store::MAX_IMPORT_REPRESENTATIONS
                {
                    return Err(ImportError::service("batch_too_large"));
                }
                let candidate_bytes = candidate
                    .owned_allocation_bytes()
                    .ok_or_else(|| ImportError::service("batch_too_large"))?;
                let candidate_total = batch_bytes.checked_add(candidate_bytes);
                if candidate_total.is_none_or(|bytes| bytes > MAX_IMPORT_BATCH_BYTES)
                    && batch.is_empty()
                {
                    #[cfg(test)]
                    self.worker_policy.before_failure();
                    if let Err(error) = self
                        .store
                        .fail_import(run_id, generation, "batch_too_large")
                        .await
                    {
                        return Err(map_store_error(error));
                    }
                    return Err(ImportError::service("batch_too_large"));
                }
                if candidate_total.is_none_or(|bytes| bytes > MAX_IMPORT_BATCH_BYTES) {
                    pending = Some(candidate);
                    break;
                }
                batch_bytes = candidate_total.expect("the checked batch total was validated");
                batch.push(candidate);
            }
            if batch.is_empty() {
                break;
            }
            let expected = batch.len() as u64;
            let outcome = match self
                .store
                .import_batch(operation_permit, run_id, generation, batch)
                .await
            {
                Ok(outcome) => outcome,
                Err(StoreError::ImportWorkerSuperseded) => {
                    return Ok(WorkerCompletion::Superseded);
                }
                Err(error) => {
                    let reason = store_error_reason(&error);
                    if reason == "private_storage_unavailable" {
                        return Err(ImportError::service(reason));
                    }
                    if let Err(error) = self
                        .store
                        .fail_import(run_id, generation, "store_failure")
                        .await
                    {
                        return Err(map_store_error(error));
                    }
                    return Err(ImportError::service("store_failure"));
                }
            };
            #[cfg(test)]
            let mut outcome = outcome;
            #[cfg(test)]
            if self
                .worker_policy
                .force_checkpoint_mismatch_once
                .swap(false, Ordering::AcqRel)
            {
                outcome.processed_candidates = outcome.processed_candidates.saturating_sub(1);
            }
            if outcome.processed_candidates != expected {
                #[cfg(test)]
                self.worker_policy.before_failure();
                if let Err(error) = self
                    .store
                    .fail_import(run_id, generation, "checkpoint_failure")
                    .await
                {
                    return Err(map_store_error(error));
                }
                return Err(ImportError::service("checkpoint_failure"));
            }
            completed_batches += 1;
            if (pending.is_some() || remaining.size_hint().0 != 0)
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
            Err(error) => {
                let reason = store_error_reason(&error);
                if reason == "private_storage_unavailable" {
                    return Err(ImportError::service(reason));
                }
                if let Err(error) = self
                    .store
                    .fail_import(run_id, generation, "finalization_failure")
                    .await
                {
                    return Err(map_store_error(error));
                }
                return Err(ImportError::service("finalization_failure"));
            }
        }
        Ok(WorkerCompletion::Completed)
    }
}

impl Drop for ImportService {
    fn drop(&mut self) {
        if Arc::strong_count(&self.owner_lifetime) != 1 {
            return;
        }
        self.runtime
            .prepared_sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove_owner(self.owner);
    }
}

#[derive(Clone)]
struct AdmissionBudget {
    ledger: Arc<Mutex<AdmissionLedger>>,
    limits: ImportAdmissionLimits,
}

impl AdmissionBudget {
    fn new(limits: ImportAdmissionLimits) -> Self {
        Self {
            ledger: Arc::new(Mutex::new(AdmissionLedger {
                source_count: 0,
                retained_bytes: limits.runtime_control_bytes,
            })),
            limits,
        }
    }

    fn try_reserve(&self) -> Result<Option<AdmissionReservation>, ImportError> {
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?;
        let Some(retained_bytes) = ledger
            .retained_bytes
            .checked_add(self.limits.max_source_bytes)
        else {
            return Ok(None);
        };
        if ledger.source_count >= self.limits.max_sources
            || retained_bytes > self.limits.prepared_cache_bytes
        {
            return Ok(None);
        }
        ledger.source_count += 1;
        ledger.retained_bytes = retained_bytes;
        Ok(Some(AdmissionReservation {
            ledger: self.ledger.clone(),
            limits: self.limits,
            retained_bytes: self.limits.max_source_bytes,
        }))
    }
}

struct AdmissionLedger {
    source_count: usize,
    retained_bytes: usize,
}

pub(crate) struct AdmissionReservation {
    ledger: Arc<Mutex<AdmissionLedger>>,
    limits: ImportAdmissionLimits,
    retained_bytes: usize,
}

impl AdmissionReservation {
    fn resize(&mut self, retained_bytes: usize) -> Result<(), ImportError> {
        if retained_bytes > self.limits.max_source_bytes {
            return Err(ImportError::service("analysis_too_large"));
        }
        let mut ledger = self
            .ledger
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?;
        let without_reservation = ledger
            .retained_bytes
            .checked_sub(self.retained_bytes)
            .ok_or_else(|| ImportError::service("analysis_unavailable"))?;
        let resized_total = without_reservation
            .checked_add(retained_bytes)
            .ok_or_else(|| ImportError::service("analysis_capacity_full"))?;
        if resized_total > self.limits.prepared_cache_bytes {
            return Err(ImportError::service("analysis_capacity_full"));
        }
        ledger.retained_bytes = resized_total;
        self.retained_bytes = retained_bytes;
        Ok(())
    }
}

impl Drop for AdmissionReservation {
    fn drop(&mut self) {
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger.source_count = ledger.source_count.saturating_sub(1);
        ledger.retained_bytes = ledger.retained_bytes.saturating_sub(self.retained_bytes);
    }
}

struct PreparedSourceEnvelope {
    source: PreparedSource,
    _reservation: AdmissionReservation,
}

struct PreparedSessions {
    sessions: BTreeMap<Uuid, PreparedSession>,
    insertion_order: VecDeque<Uuid>,
    ttl: Duration,
    control_bytes: usize,
}

impl Default for PreparedSessions {
    fn default() -> Self {
        Self::with_control(MAX_PREPARED_RUNTIME_CONTROL_BYTES)
    }
}

impl PreparedSessions {
    fn with_control(control_bytes: usize) -> Self {
        assert!(control_bytes >= MAX_PREPARED_SESSION_CONTROL_BYTES);
        Self {
            sessions: BTreeMap::new(),
            insertion_order: VecDeque::new(),
            ttl: PREPARED_SESSION_TTL,
            control_bytes,
        }
    }

    fn remove_owner(&mut self, owner: ServiceOwnerToken) {
        while let Some(analysis_id) = self
            .sessions
            .iter()
            .find_map(|(analysis_id, session)| (session.owner == owner).then_some(*analysis_id))
        {
            self.sessions.remove(&analysis_id);
            self.insertion_order
                .retain(|candidate| *candidate != analysis_id);
        }
    }

    fn insert(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        source: PreparedSourceEnvelope,
        now: Instant,
    ) -> Uuid {
        self.insert_with_hook(owner, analysis_id, source, now, |_, _, _| {})
    }

    fn insert_with_hook(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        source: PreparedSourceEnvelope,
        now: Instant,
        before_allocation: impl FnOnce(usize, usize, bool),
    ) -> Uuid {
        self.evict_expired(now);
        while self.sessions.len() >= PREPARED_SESSION_CAPACITY {
            if !self.evict_oldest_available() {
                break;
            }
        }
        before_allocation(
            MAX_PREPARED_SESSION_CONTROL_BYTES,
            self.control_bytes,
            self.sessions.contains_key(&analysis_id),
        );
        self.insert_session(
            analysis_id,
            PreparedSession {
                owner,
                created_at: now,
                state: PreparedSessionState::Available(source),
            },
        );
        self.insertion_order.push_back(analysis_id);
        analysis_id
    }

    fn insert_session(&mut self, analysis_id: Uuid, session: PreparedSession) {
        assert!(self.control_bytes >= MAX_PREPARED_SESSION_CONTROL_BYTES);
        self.sessions.insert(analysis_id, session);
    }

    fn begin(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        now: Instant,
    ) -> BeginPreparedSession {
        self.evict_expired(now);
        if !self
            .sessions
            .get(&analysis_id)
            .is_some_and(|session| session.owner == owner)
        {
            return BeginPreparedSession::Missing;
        }
        let Some(session) = self.sessions.remove(&analysis_id) else {
            return BeginPreparedSession::Missing;
        };
        match session.state {
            PreparedSessionState::Available(source) => {
                self.insertion_order
                    .retain(|candidate| *candidate != analysis_id);
                let completion = Arc::new(PersistenceCompletion::default());
                self.insert_session(
                    analysis_id,
                    PreparedSession {
                        owner: session.owner,
                        created_at: session.created_at,
                        state: PreparedSessionState::Persisting(completion.clone()),
                    },
                );
                BeginPreparedSession::Start { source, completion }
            }
            PreparedSessionState::Persisting(completion) => {
                self.insert_session(
                    analysis_id,
                    PreparedSession {
                        owner: session.owner,
                        created_at: session.created_at,
                        state: PreparedSessionState::Persisting(completion.clone()),
                    },
                );
                BeginPreparedSession::Wait(completion)
            }
            PreparedSessionState::Started(completion) => {
                self.insert_session(
                    analysis_id,
                    PreparedSession {
                        owner: session.owner,
                        created_at: session.created_at,
                        state: PreparedSessionState::Started(completion.clone()),
                    },
                );
                BeginPreparedSession::Wait(completion)
            }
            PreparedSessionState::Completed(handle) => {
                self.insert_session(
                    analysis_id,
                    PreparedSession {
                        owner: session.owner,
                        created_at: session.created_at,
                        state: PreparedSessionState::Completed(handle),
                    },
                );
                BeginPreparedSession::Ready(handle)
            }
        }
    }

    fn mark_started(&mut self, owner: ServiceOwnerToken, analysis_id: Uuid) -> bool {
        if !self
            .sessions
            .get(&analysis_id)
            .is_some_and(|session| session.owner == owner)
        {
            return false;
        }
        let Some(session) = self.sessions.remove(&analysis_id) else {
            return false;
        };
        let PreparedSessionState::Persisting(completion) = session.state else {
            self.insert_session(analysis_id, session);
            return false;
        };
        self.insert_session(
            analysis_id,
            PreparedSession {
                owner: session.owner,
                created_at: session.created_at,
                state: PreparedSessionState::Started(completion),
            },
        );
        true
    }

    fn restore(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        source: PreparedSourceEnvelope,
        now: Instant,
    ) {
        self.sessions.remove(&analysis_id);
        self.insert_session(
            analysis_id,
            PreparedSession {
                owner,
                created_at: now,
                state: PreparedSessionState::Available(source),
            },
        );
        if !self.insertion_order.contains(&analysis_id) {
            self.insertion_order.push_back(analysis_id);
        }
    }

    fn finish_persisting(&mut self, analysis_id: Uuid) {
        self.sessions.remove(&analysis_id);
        self.insertion_order
            .retain(|candidate| *candidate != analysis_id);
    }

    fn finish_started(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        completed: bool,
        now: Instant,
    ) {
        if !self
            .sessions
            .get(&analysis_id)
            .is_some_and(|session| session.owner == owner)
        {
            return;
        }
        if completed {
            self.finish_completed(
                owner,
                analysis_id,
                ImportRunHandle {
                    run_id: analysis_id,
                },
                now,
            );
        } else {
            self.finish_persisting(analysis_id);
        }
    }

    fn finish_completed(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        handle: ImportRunHandle,
        now: Instant,
    ) {
        if !self
            .sessions
            .get(&analysis_id)
            .is_some_and(|session| session.owner == owner)
        {
            return;
        }
        self.insert_session(
            analysis_id,
            PreparedSession {
                owner,
                created_at: now,
                state: PreparedSessionState::Completed(handle),
            },
        );
        if !self.insertion_order.contains(&analysis_id) {
            self.insertion_order.push_back(analysis_id);
        }
    }

    fn discard(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        now: Instant,
    ) -> Result<(), ImportError> {
        self.evict_expired(now);
        if !self
            .sessions
            .get(&analysis_id)
            .is_some_and(|session| session.owner == owner)
        {
            return Err(ImportError::service("analysis_not_found"));
        }
        match self
            .sessions
            .get(&analysis_id)
            .map(|session| &session.state)
        {
            Some(PreparedSessionState::Available(_)) => {
                self.finish_persisting(analysis_id);
                Ok(())
            }
            Some(PreparedSessionState::Persisting(_)) => {
                Err(ImportError::service("analysis_in_progress"))
            }
            Some(PreparedSessionState::Started(_)) => {
                Err(ImportError::service("analysis_in_progress"))
            }
            Some(PreparedSessionState::Completed(_)) => {
                self.finish_persisting(analysis_id);
                Ok(())
            }
            None => Err(ImportError::service("analysis_not_found")),
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        while let Some(analysis_id) = self.insertion_order.iter().copied().find(|analysis_id| {
            self.sessions.get(analysis_id).is_some_and(|session| {
                matches!(
                    session.state,
                    PreparedSessionState::Available(_) | PreparedSessionState::Completed(_)
                ) && now.saturating_duration_since(session.created_at) >= self.ttl
            })
        }) {
            self.finish_persisting(analysis_id);
        }
    }

    fn evict_oldest_available(&mut self) -> bool {
        while let Some(analysis_id) = self.insertion_order.pop_front() {
            if self.sessions.get(&analysis_id).is_some_and(|session| {
                matches!(
                    session.state,
                    PreparedSessionState::Available(_) | PreparedSessionState::Completed(_)
                )
            }) {
                self.finish_persisting(analysis_id);
                return true;
            }
        }
        false
    }
}

struct PreparedSession {
    owner: ServiceOwnerToken,
    created_at: Instant,
    state: PreparedSessionState,
}

enum PreparedSessionState {
    Available(PreparedSourceEnvelope),
    Persisting(Arc<PersistenceCompletion>),
    Started(Arc<PersistenceCompletion>),
    Completed(ImportRunHandle),
}

const _: () = assert!(
    PREPARED_SESSION_CAPACITY
        * (size_of::<Uuid>() + size_of::<PreparedSession>() + size_of::<Uuid>() + 128)
        <= MAX_PREPARED_SESSION_CONTROL_BYTES
);

enum BeginPreparedSession {
    Start {
        source: PreparedSourceEnvelope,
        completion: Arc<PersistenceCompletion>,
    },
    Wait(Arc<PersistenceCompletion>),
    Ready(ImportRunHandle),
    Missing,
}

#[derive(Default)]
struct PersistenceCompletion {
    result: Mutex<Option<Result<ImportRunHandle, &'static str>>>,
    notified: tokio::sync::Notify,
}

impl PersistenceCompletion {
    async fn wait(&self) -> Result<ImportRunHandle, ImportError> {
        loop {
            let notified = self.notified.notified();
            if let Some(result) = self
                .result
                .lock()
                .map_err(|_| ImportError::service("analysis_unavailable"))?
                .as_ref()
                .copied()
            {
                return result.map_err(ImportError::service);
            }
            notified.await;
        }
    }

    fn finish(&self, result: Result<ImportRunHandle, &'static str>) {
        if let Ok(mut slot) = self.result.lock() {
            *slot = Some(result);
            self.notified.notify_waiters();
        }
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

    fn retained_bytes(&self) -> usize {
        let mut bytes = PREPARED_SESSION_OVERHEAD_BYTES
            .saturating_add(size_of::<Self>())
            .saturating_add(
                self.candidates
                    .capacity()
                    .saturating_mul(size_of::<ImportCandidate>()),
            )
            .saturating_add(
                self.failure_counts
                    .capacity()
                    .saturating_mul(size_of::<ImportFailureCount>()),
            );
        for candidate in &self.candidates {
            let capture = &candidate.capture;
            bytes = bytes
                .saturating_add(capture.primary_mime.capacity())
                .saturating_add(
                    capture
                        .representations
                        .capacity()
                        .saturating_mul(size_of::<clipboard_core::RepresentationInput>()),
                )
                .saturating_add(option_string_capacity(&capture.source_app_id))
                .saturating_add(option_string_capacity(&capture.source_app_name))
                .saturating_add(option_string_capacity(&candidate.search_ocr))
                .saturating_add(option_string_capacity(&candidate.source_application_path));
            for representation in &capture.representations {
                bytes = bytes
                    .saturating_add(representation.format_id.capacity())
                    .saturating_add(representation.bytes.as_ref().map_or(0, Vec::capacity))
                    .saturating_add(option_string_capacity(&representation.missing_ref));
            }
        }
        for failure in &self.failure_counts {
            bytes = bytes.saturating_add(failure.reason_code.capacity());
        }
        bytes
    }
}

fn option_string_capacity(value: &Option<String>) -> usize {
    value.as_ref().map_or(0, String::capacity)
}

fn prepare_source_with_permit(
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<PreparedSource, ImportError> {
    prepare_source_with_hook(path, permit, limits, || {})
}

fn prepare_source_with_hook(
    path: &Path,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
    after_initial_detection: impl FnOnce(),
) -> Result<PreparedSource, ImportError> {
    let detected = detect_export_with_permit(path, permit, limits)?;
    after_initial_detection();
    let report = parse_detected_export_report_with_permit(&detected, permit, limits);
    let verified = detect_export_with_permit(path, permit, limits);
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
    let failure_counts = aggregate_failures(&report, limits.source_control_bytes)?;
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

fn aggregate_failures(
    report: &ImportParseReport,
    source_control_bytes: usize,
) -> Result<Vec<ImportFailureCount>, ImportError> {
    aggregate_failures_with_hook(report, source_control_bytes, |_, _| {})
}

fn aggregate_failures_with_hook(
    report: &ImportParseReport,
    source_control_bytes: usize,
    before_allocation: impl FnOnce(usize, usize),
) -> Result<Vec<ImportFailureCount>, ImportError> {
    let mut counts = [("", 0_u64); MAX_IMPORT_FAILURE_KINDS];
    let mut used = 0_usize;
    for failure in &report.failures {
        if let Some((_, count)) = counts[..used]
            .iter_mut()
            .find(|(reason, _)| *reason == failure.reason)
        {
            *count = count
                .checked_add(1)
                .ok_or_else(|| ImportError::service("source_too_large"))?;
            continue;
        }
        let slot = counts
            .get_mut(used)
            .ok_or_else(|| ImportError::service("analysis_too_large"))?;
        *slot = (failure.reason, 1);
        used += 1;
    }
    counts[..used].sort_unstable_by_key(|(reason, _)| *reason);
    let required = used
        .checked_mul(size_of::<ImportFailureCount>())
        .and_then(|bytes| {
            counts[..used]
                .iter()
                .try_fold(bytes, |total, (reason, _)| total.checked_add(reason.len()))
        })
        .ok_or_else(|| ImportError::service("analysis_too_large"))?;
    let reserved = source_control_bytes.min(MAX_PREPARED_FAILURE_CONTROL_BYTES);
    if required > reserved {
        return Err(ImportError::service("analysis_too_large"));
    }
    before_allocation(required, reserved);
    let mut failures = Vec::with_capacity(used);
    failures.extend(
        counts[..used]
            .iter()
            .map(|(reason_code, count)| ImportFailureCount {
                reason_code: (*reason_code).to_owned(),
                count: *count,
            }),
    );
    Ok(failures)
}

fn store_source(source: ImportSource) -> ImportSourceKind {
    match source {
        ImportSource::Raycast => ImportSourceKind::Raycast,
        ImportSource::SuperCmd => ImportSourceKind::SuperCmd,
    }
}

fn store_candidate(candidate_offset: u64, candidate: ImportCandidate) -> StoreImportCandidate {
    StoreImportCandidate {
        candidate_offset,
        record_fingerprint: candidate.record_fingerprint,
        capture: candidate.capture,
        search_ocr: candidate.search_ocr,
        source_app_original: candidate.source_application_path,
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
    ImportError::service(store_error_reason(&error))
}

fn store_error_reason(error: &StoreError) -> &'static str {
    match error {
        StoreError::PrivateStorageUnavailable
        | StoreError::Cas(clipboard_store::CasError::PrivateStorageUnavailable) => {
            "private_storage_unavailable"
        }
        StoreError::ImportRunNotFound => "run_not_found",
        StoreError::ImportSourceMismatch => "source_mismatch",
        StoreError::ImportRunConflict => "run_conflict",
        StoreError::ImportRunNotResumable => "run_not_resumable",
        StoreError::ImportCheckpointMismatch => "checkpoint_failure",
        StoreError::ImportWorkerSuperseded => "worker_superseded",
        StoreError::ImportInvariant => "invalid_run_state",
        StoreError::InvalidImportInput | StoreError::ImportBatchTooLarge => "invalid_import",
        _ => "store_failure",
    }
}

fn import_error_reason(error: &ImportError) -> &'static str {
    match error {
        ImportError::Export { reason, .. }
        | ImportError::Record { reason, .. }
        | ImportError::Service { reason } => reason,
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, sync::Arc};

    #[cfg(unix)]
    use clipboard_store::{StorageBoundaryLease, StoreConfig, StoreHandle};
    use serde_json::json;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn assert_runtime_envelope(runtime: &ImportRuntime) -> (usize, usize) {
        let (retained_bytes, operation_bytes) = runtime.snapshot();
        assert!(retained_bytes <= runtime.limits.prepared_cache_bytes);
        assert!(operation_bytes <= runtime.limits.operation_bytes);
        assert!(
            retained_bytes
                .checked_add(operation_bytes)
                .is_some_and(|bytes| bytes <= runtime.limits.runtime_bytes)
        );
        (retained_bytes, operation_bytes)
    }

    async fn runtime_memory_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        static TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        TEST_LOCK.lock().await
    }

    #[tokio::test]
    async fn runtime_memory_precharges_fixed_control_before_any_source() {
        let _test_guard = runtime_memory_test_guard().await;
        let limits = ImportAdmissionLimits::default();
        let budget = AdmissionBudget::new(limits);
        let ledger = budget.ledger.lock().unwrap();

        assert_eq!(ledger.retained_bytes, MAX_PREPARED_RUNTIME_CONTROL_BYTES);
        assert_eq!(ledger.source_count, 0);
    }

    #[tokio::test]
    async fn runtime_memory_parser_payload_carves_source_control_before_growth() {
        let _test_guard = runtime_memory_test_guard().await;
        let runtime = ImportRuntime::with_limits(ImportAdmissionLimits::default());
        let parse_limits = runtime.parse_limits();

        assert_eq!(
            parse_limits
                .source_bytes
                .checked_add(parse_limits.source_control_bytes),
            Some(MAX_PREPARED_SOURCE_BYTES)
        );
        assert_eq!(
            parse_limits.source_control_bytes,
            MAX_PREPARED_SOURCE_CONTROL_BYTES
        );
        assert!(
            crate::detect::MAX_IMPORT_PATH_CONTROL_BYTES
                + crate::supercmd::MAX_AUXILIARY_TRAVERSAL_CONTROL_BYTES
                + MAX_PREPARED_FAILURE_CONTROL_BYTES
                + crate::MAX_IMPORT_ANALYSIS_CONTROL_BYTES
                + PREPARED_SESSION_OVERHEAD_BYTES
                <= parse_limits.source_control_bytes
        );
        assert!(parse_limits.source_bytes < MAX_PREPARED_SOURCE_BYTES);
    }

    #[test]
    fn failure_summary_capacity_is_authorized_before_growth() {
        let mut report = ImportParseReport::with_source_limit(1, 4096);
        report
            .push(Err(crate::record_failure(
                ImportSource::Raycast,
                1,
                "invalid_record",
            )))
            .unwrap();
        let authorized = std::cell::Cell::new(false);

        let failures = aggregate_failures_with_hook(
            &report,
            MAX_PREPARED_SOURCE_CONTROL_BYTES,
            |required, reserved| {
                assert!(required > 0);
                assert!(required <= reserved);
                authorized.set(true);
            },
        )
        .unwrap();

        assert!(authorized.get());
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn prepared_session_capacity_is_authorized_before_map_growth() {
        let budget = AdmissionBudget::new(ImportAdmissionLimits::default());
        let mut sessions =
            PreparedSessions::with_control(ImportAdmissionLimits::default().runtime_control_bytes);
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let analysis_id = Uuid::now_v7();
        let source = admitted_source(&mut sessions, &budget, empty_source(1));
        let authorized = std::cell::Cell::new(false);

        sessions.insert_with_hook(
            owner,
            analysis_id,
            source,
            Instant::now(),
            |required, reserved, already_present| {
                assert!(!already_present);
                assert!(required > 0);
                assert!(required <= reserved);
                authorized.set(true);
            },
        );

        assert!(authorized.get());
        assert!(sessions.sessions.contains_key(&analysis_id));
    }

    #[tokio::test]
    async fn runtime_memory_services_share_eviction_but_not_session_authority() {
        let _test_guard = runtime_memory_test_guard().await;
        let first_database = tempfile::tempdir().unwrap();
        let second_database = tempfile::tempdir().unwrap();
        let first_export = synthetic_export();
        let second_export = synthetic_export();
        let runtime = ImportRuntime::with_limits(ImportAdmissionLimits::new(
            1,
            64 * 1024,
            16 * 1024,
            80 * 1024,
            16 * 1024,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 80 * 1024,
        ));
        let first = ImportService::with_runtime(
            StoreHandle::open(StoreConfig::new(
                first_database.path().join("history.sqlite"),
            ))
            .unwrap(),
            runtime.clone(),
        );
        let second = ImportService::with_runtime(
            StoreHandle::open(StoreConfig::new(
                second_database.path().join("history.sqlite"),
            ))
            .unwrap(),
            runtime.clone(),
        );

        let first_analysis = first.analyze(first_export.path()).unwrap();
        let (first_retained, first_operation) = assert_runtime_envelope(&runtime);
        assert!(first_retained > 0);
        assert_eq!(first_operation, 0);

        let second_analysis = second.analyze(second_export.path()).unwrap();
        assert_runtime_envelope(&runtime);
        assert!(matches!(
            first.begin(first_analysis.analysis_id).await,
            Err(ImportError::Service {
                reason: "analysis_not_found"
            })
        ));
        assert!(matches!(
            second.begin(first_analysis.analysis_id).await,
            Err(ImportError::Service {
                reason: "analysis_not_found"
            })
        ));
        assert!(matches!(
            second.discard_analysis(first_analysis.analysis_id),
            Err(ImportError::Service {
                reason: "analysis_not_found"
            })
        ));
        second
            .discard_analysis(second_analysis.analysis_id)
            .unwrap();
        assert_eq!(
            assert_runtime_envelope(&runtime),
            (runtime.limits.runtime_control_bytes, 0)
        );
    }

    #[tokio::test]
    async fn runtime_memory_completion_keeps_only_an_owner_scoped_tombstone() {
        let _test_guard = runtime_memory_test_guard().await;
        let database = tempfile::tempdir().unwrap();
        let export = synthetic_export();
        let runtime = ImportRuntime::with_limits(ImportAdmissionLimits::new(
            2,
            64 * 1024,
            16 * 1024,
            96 * 1024,
            16 * 1024,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 96 * 1024,
        ));
        let store =
            StoreHandle::open(StoreConfig::new(database.path().join("history.sqlite"))).unwrap();
        let owner = ImportService::with_runtime(store.clone(), runtime.clone());
        let stranger = ImportService::with_runtime(store, runtime.clone());
        let analysis = owner.analyze(export.path()).unwrap();
        let handle = owner.begin(analysis.analysis_id).await.unwrap();

        for _ in 0..50_000 {
            if owner.status(handle.run_id).unwrap().state != ImportRunState::Running {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            owner.status(handle.run_id).unwrap().state,
            ImportRunState::Completed
        );
        for _ in 0..50_000 {
            if runtime.snapshot().0 == runtime.limits.runtime_control_bytes {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            assert_runtime_envelope(&runtime),
            (runtime.limits.runtime_control_bytes, 0)
        );
        assert_eq!(owner.begin(analysis.analysis_id).await.unwrap(), handle);
        assert!(matches!(
            stranger.begin(analysis.analysis_id).await,
            Err(ImportError::Service {
                reason: "analysis_not_found"
            })
        ));
        assert!(matches!(
            stranger.discard_analysis(analysis.analysis_id),
            Err(ImportError::Service {
                reason: "analysis_not_found"
            })
        ));
        owner.discard_analysis(analysis.analysis_id).unwrap();
        assert!(matches!(
            owner.begin(analysis.analysis_id).await,
            Err(ImportError::Service {
                reason: "analysis_not_found"
            })
        ));
    }

    #[tokio::test]
    async fn runtime_memory_persistence_failure_restores_the_exact_reserved_snapshot() {
        let _test_guard = runtime_memory_test_guard().await;
        let database = tempfile::tempdir().unwrap();
        let export = synthetic_export();
        let limits = ImportAdmissionLimits::new(
            2,
            64 * 1024,
            16 * 1024,
            96 * 1024,
            16 * 1024,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 96 * 1024,
        );
        let service = ImportService::with_worker_policy_and_limits(
            StoreHandle::open(StoreConfig::new(database.path().join("history.sqlite"))).unwrap(),
            ImportWorkerPolicy::fail_next_persistence(),
            limits,
        );
        let runtime = service.runtime.clone();
        let analysis = service.analyze(export.path()).unwrap();
        let before = assert_runtime_envelope(&runtime);
        assert!(before.0 > 0);

        let error = service.begin(analysis.analysis_id).await.unwrap_err();

        assert!(matches!(
            error,
            ImportError::Service {
                reason: "store_failure"
            }
        ));
        assert_eq!(assert_runtime_envelope(&runtime), before);
        let handle = service.begin(analysis.analysis_id).await.unwrap();
        for _ in 0..50_000 {
            if service.status(handle.run_id).unwrap().state != ImportRunState::Running {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            service.status(handle.run_id).unwrap().state,
            ImportRunState::Completed
        );
        for _ in 0..50_000 {
            if runtime.snapshot().0 == runtime.limits.runtime_control_bytes {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            assert_runtime_envelope(&runtime),
            (runtime.limits.runtime_control_bytes, 0)
        );
    }

    #[tokio::test]
    async fn runtime_memory_last_service_drop_releases_owned_available_sessions() {
        let _test_guard = runtime_memory_test_guard().await;
        let database = tempfile::tempdir().unwrap();
        let export = synthetic_export();
        let runtime = ImportRuntime::with_limits(ImportAdmissionLimits::new(
            2,
            64 * 1024,
            16 * 1024,
            96 * 1024,
            16 * 1024,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 96 * 1024,
        ));
        let service = ImportService::with_runtime(
            StoreHandle::open(StoreConfig::new(database.path().join("history.sqlite"))).unwrap(),
            runtime.clone(),
        );
        service.analyze(export.path()).unwrap();
        assert!(assert_runtime_envelope(&runtime).0 > 0);

        drop(service);

        assert_eq!(
            assert_runtime_envelope(&runtime),
            (runtime.limits.runtime_control_bytes, 0)
        );
    }

    #[test]
    fn private_storage_store_errors_keep_their_service_reason() {
        for error in [
            StoreError::PrivateStorageUnavailable,
            StoreError::Cas(clipboard_store::CasError::PrivateStorageUnavailable),
        ] {
            assert!(matches!(
                map_store_error(error),
                ImportError::Service {
                    reason: "private_storage_unavailable"
                }
            ));
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn begin_preserves_private_storage_through_persistence_handoff() {
        let _test_guard = runtime_memory_test_guard().await;
        let (directory, store, data_dir) = leased_store();
        let service = ImportService::new(store);
        let export = synthetic_export();
        let analysis = service.analyze(export.path()).unwrap();

        make_blob_storage_private(&data_dir);
        let error = service.begin(analysis.analysis_id).await.unwrap_err();
        restore_blob_storage(&data_dir);

        assert_private_service_error(error);
        drop(directory);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_storage_errors_survive_import_lifecycle_boundaries() {
        let _test_guard = runtime_memory_test_guard().await;
        let (directory, store, data_dir) = leased_store();
        let service = ImportService::new(store.clone());
        let export = synthetic_export();

        make_blob_storage_private(&data_dir);
        let persistence_error = service.run_to_completion(export.path()).await.unwrap_err();
        assert_private_service_error(persistence_error);
        restore_blob_storage(&data_dir);

        let resumable = service.prepare(export.path()).unwrap();
        let run = service
            .persist_run(Uuid::now_v7(), &resumable.source)
            .await
            .unwrap();
        make_blob_storage_private(&data_dir);
        let resume_error = service.resume(run.run_id, export.path()).await.unwrap_err();
        assert_private_service_error(resume_error);
        restore_blob_storage(&data_dir);
        assert_unchanged_failure_accounting(&store, run.run_id);

        let source = source_with_payload(1);
        let run = service.persist_run(Uuid::now_v7(), &source).await.unwrap();

        make_blob_storage_private(&data_dir);
        let batch_error = match service
            .run_worker(
                run.run_id,
                run.generation,
                admitted_source_for_worker(source_with_payload(1)),
                0,
            )
            .await
        {
            Ok(_) => panic!("private storage batch failure must be returned"),
            Err(error) => error,
        };
        assert_private_service_error(batch_error);
        restore_blob_storage(&data_dir);
        assert_unchanged_failure_accounting(&store, run.run_id);

        let source = empty_source(9);
        let run = service.persist_run(Uuid::now_v7(), &source).await.unwrap();
        make_blob_storage_private(&data_dir);
        let finalize_error = match service
            .run_worker(
                run.run_id,
                run.generation,
                admitted_source_for_worker(empty_source(9)),
                0,
            )
            .await
        {
            Ok(_) => panic!("private storage finalization failure must be returned"),
            Err(error) => error,
        };
        assert_private_service_error(finalize_error);
        restore_blob_storage(&data_dir);
        assert_unchanged_failure_accounting(&store, run.run_id);
        drop(directory);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn private_storage_from_failure_marking_overrides_generic_worker_reasons() {
        let _test_guard = runtime_memory_test_guard().await;
        let (directory, store, data_dir) = leased_store();
        let batch_service = ImportService::with_worker_policy(
            store.clone(),
            ImportWorkerPolicy::fail_marking_with_hook(private_storage_hook(data_dir.clone())),
        );
        let oversized = source_with_payload(MAX_IMPORT_BATCH_BYTES + 1);
        let run = batch_service
            .persist_run(Uuid::now_v7(), &oversized)
            .await
            .unwrap();
        let error = match batch_service
            .run_worker(
                run.run_id,
                run.generation,
                admitted_source_for_worker(source_with_payload(MAX_IMPORT_BATCH_BYTES + 1)),
                0,
            )
            .await
        {
            Ok(_) => panic!("private failure-marking error must be returned"),
            Err(error) => error,
        };
        assert_private_service_error(error);
        restore_blob_storage(&data_dir);
        assert_unchanged_failure_accounting(&store, run.run_id);

        let checkpoint_service = ImportService::with_worker_policy(
            store.clone(),
            ImportWorkerPolicy::checkpoint_mismatch_with_hook(private_storage_hook(
                data_dir.clone(),
            )),
        );
        let source = source_with_payload(1);
        let run = checkpoint_service
            .persist_run(Uuid::now_v7(), &source)
            .await
            .unwrap();
        let error = match checkpoint_service
            .run_worker(
                run.run_id,
                run.generation,
                admitted_source_for_worker(source_with_payload(1)),
                0,
            )
            .await
        {
            Ok(_) => panic!("private failure-marking error must be returned"),
            Err(error) => error,
        };
        assert_private_service_error(error);
        restore_blob_storage(&data_dir);
        assert_unchanged_failure_accounting(&store, run.run_id);
        drop(directory);
    }

    #[cfg(unix)]
    fn leased_store() -> (tempfile::TempDir, StoreHandle, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let data_dir = directory.path().join("synthetic-leased-data");
        fs::create_dir(&data_dir).unwrap();
        let config = StoreConfig::new(data_dir.join("history.sqlite"))
            .with_blob_root(data_dir.join("blobs"));
        let lease = Arc::new(StorageBoundaryLease::create_writer(&config).unwrap());
        let store = StoreHandle::open(config.with_storage_boundary(lease)).unwrap();
        (directory, store, data_dir)
    }

    #[cfg(unix)]
    fn synthetic_export() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("clipboard.json"),
            serde_json::to_vec(&[json!({
                "createdAt": "2026-08-22T12:00:00Z",
                "modifiedAt": "2026-08-22T12:00:00Z",
                "category": "text",
                "copyCount": 1,
                "text": "synthetic private storage lifecycle",
            })])
            .unwrap(),
        )
        .unwrap();
        directory
    }

    #[cfg(unix)]
    fn admitted_source_for_worker(source: PreparedSource) -> PreparedSourceEnvelope {
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            1,
            MAX_PREPARED_SOURCE_BYTES,
            MAX_PREPARED_SOURCE_CONTROL_BYTES,
            MAX_PREPARED_SOURCE_BYTES + MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_RUNTIME_BYTES,
        ));
        admitted_source(&mut PreparedSessions::default(), &budget, source)
    }

    #[cfg(unix)]
    fn private_storage_hook(data_dir: PathBuf) -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(move || make_blob_storage_private(&data_dir))
    }

    #[cfg(unix)]
    fn make_blob_storage_private(data_dir: &std::path::Path) {
        fs::set_permissions(data_dir.join("blobs"), fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[cfg(unix)]
    fn restore_blob_storage(data_dir: &std::path::Path) {
        fs::set_permissions(data_dir.join("blobs"), fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    fn assert_private_service_error(error: ImportError) {
        assert!(matches!(
            error,
            ImportError::Service {
                reason: "private_storage_unavailable"
            }
        ));
    }

    #[cfg(unix)]
    fn assert_unchanged_failure_accounting(store: &StoreHandle, run_id: Uuid) {
        let status = store.import_status(run_id).unwrap();
        assert_eq!(status.failed_records, 0);
        assert_eq!(status.error_code, None);
    }

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

        let gate = ImportOperationGate::with_capacity(MAX_IMPORT_OPERATION_BYTES).unwrap();
        let permit = gate.acquire_blocking().unwrap();
        let result =
            prepare_source_with_hook(export.path(), &permit, ImportParseLimits::default(), || {
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
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            PREPARED_SESSION_CAPACITY,
            32 * 1024,
            8 * 1024,
            PREPARED_SESSION_CAPACITY * 32 * 1024,
            16 * 1024,
            4 * 1024,
            PREPARED_SESSION_CAPACITY * 32 * 1024 + 4 * 1024,
        ));
        let now = Instant::now();
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let mut first = None;
        let mut newest = None;
        for index in 0..=PREPARED_SESSION_CAPACITY {
            let fingerprint_byte = u8::try_from(index).unwrap();
            let analysis_id = Uuid::now_v7();
            let source = empty_source(fingerprint_byte);
            let envelope = admitted_source(&mut sessions, &budget, source);
            sessions.insert(
                owner,
                analysis_id,
                envelope,
                now + Duration::from_millis(index as u64),
            );
            first.get_or_insert(analysis_id);
            newest = Some(analysis_id);
        }

        assert!(!sessions.sessions.contains_key(&first.unwrap()));
        assert!(sessions.sessions.contains_key(&newest.unwrap()));
        assert_eq!(sessions.sessions.len(), PREPARED_SESSION_CAPACITY);
    }

    #[test]
    fn prepared_session_cache_expires_available_entries_at_the_fixed_ttl() {
        let mut sessions = PreparedSessions::default();
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            1,
            32 * 1024,
            8 * 1024,
            48 * 1024,
            16 * 1024,
            4 * 1024,
            52 * 1024,
        ));
        let now = Instant::now();
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let analysis_id = Uuid::now_v7();
        let source = admitted_source(&mut sessions, &budget, empty_source(1));
        sessions.insert(owner, analysis_id, source, now);

        assert!(matches!(
            sessions.begin(owner, analysis_id, now + PREPARED_SESSION_TTL),
            BeginPreparedSession::Missing
        ));
        let ledger = budget.ledger.lock().unwrap();
        assert_eq!(ledger.source_count, 0);
        assert_eq!(ledger.retained_bytes, budget.limits.runtime_control_bytes);
    }

    #[test]
    fn prepared_session_cache_evicts_by_retained_byte_budget() {
        let source = empty_source(1);
        let retained_bytes = source.retained_bytes();
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            8,
            retained_bytes,
            1,
            (16_usize * 1024)
                .saturating_add(retained_bytes.saturating_mul(2))
                .saturating_sub(1),
            16 * 1024,
            retained_bytes,
            (16_usize * 1024)
                .saturating_add(retained_bytes.saturating_mul(3))
                .saturating_sub(1),
        ));
        let mut sessions = PreparedSessions::default();
        let now = Instant::now();
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let first_source = admitted_source(&mut sessions, &budget, source);
        sessions.insert(owner, first, first_source, now);

        let second_source = admitted_source(&mut sessions, &budget, empty_source(2));
        sessions.insert(owner, second, second_source, now + Duration::from_millis(1));

        assert!(!sessions.sessions.contains_key(&first));
        assert!(sessions.sessions.contains_key(&second));
        assert_eq!(budget.ledger.lock().unwrap().source_count, 1);
    }

    #[test]
    fn prepared_source_over_the_per_session_byte_limit_is_rejected() {
        let source = source_with_payload(4_096);
        let retained_bytes = source.retained_bytes();
        assert!(retained_bytes >= 4_096);
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            8,
            retained_bytes - 1,
            1,
            (16_usize * 1024).saturating_add(retained_bytes.saturating_mul(2)),
            16 * 1024,
            retained_bytes,
            (16_usize * 1024).saturating_add(retained_bytes.saturating_mul(3)),
        ));
        let mut reservation = budget.try_reserve().unwrap().unwrap();
        let error = reservation.resize(source.retained_bytes()).unwrap_err();

        assert!(matches!(
            error,
            ImportError::Service {
                reason: "analysis_too_large"
            }
        ));
    }

    fn admitted_source(
        sessions: &mut PreparedSessions,
        budget: &AdmissionBudget,
        source: PreparedSource,
    ) -> PreparedSourceEnvelope {
        let mut reservation = loop {
            if let Some(reservation) = budget.try_reserve().unwrap() {
                break reservation;
            }
            assert!(sessions.evict_oldest_available());
        };
        reservation.resize(source.retained_bytes()).unwrap();
        PreparedSourceEnvelope {
            source,
            _reservation: reservation,
        }
    }

    fn empty_source(fingerprint_byte: u8) -> PreparedSource {
        PreparedSource {
            source_kind: ImportSourceKind::Raycast,
            source_fingerprint: [fingerprint_byte; 32],
            total_records: 0,
            candidates: Vec::new(),
            failure_counts: Vec::new(),
        }
    }

    fn source_with_payload(payload_bytes: usize) -> PreparedSource {
        PreparedSource {
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [3; 32],
            total_records: 1,
            candidates: vec![ImportCandidate {
                source: ImportSource::SuperCmd,
                record_fingerprint: [4; 32],
                capture: clipboard_core::CaptureInput {
                    captured_at_ms: 1_000,
                    kind: clipboard_core::ContentKind::Image,
                    primary_mime: "image/png".to_owned(),
                    representations: vec![clipboard_core::RepresentationInput {
                        format_id: "image/png".to_owned(),
                        bytes: Some(vec![0; payload_bytes]),
                        missing_ref: None,
                    }],
                    source_app_id: None,
                    source_app_name: None,
                    source_confidence: clipboard_core::SourceConfidence::Unknown,
                    pinned: false,
                    occurrence_count: 1,
                    content_flags: clipboard_core::ContentFlags::empty(),
                    event_flags: clipboard_core::EventFlags::IMPORTED,
                },
                search_ocr: None,
                missing_payload: false,
                source_application_path: None,
            }],
            failure_counts: Vec::new(),
        }
    }
}
