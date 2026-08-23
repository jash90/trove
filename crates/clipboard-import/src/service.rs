use std::{
    mem::{align_of, size_of},
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
    RayconfigSecret,
    detect::{bounded_path_copy, detect_export_with_permit},
    parse_detected_export_report_with_permit,
};

pub const IMPORT_BATCH_SIZE: usize = clipboard_store::IMPORT_BATCH_SIZE;
pub const MAX_IMPORT_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMPORT_RECORD_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IMPORT_HEADER_BYTES: usize = 64 * 1024;
pub const MAX_IMPORT_BATCH_BYTES: usize = clipboard_store::MAX_IMPORT_BATCH_BYTES;
pub const MAX_IMPORT_AUXILIARY_BYTES: usize = 32 * 1024 * 1024;
/// Largest `.rayconfig` this will read off disk.
///
/// The whole ciphertext is held at once — CBC decrypts against the previous
/// block and PKCS7 is only readable at the end — so this competes directly with
/// the record buffer inside one 64 MiB operation permit. Twenty-four leaves
/// room for both with margin; the measured three-month export is 2.2 MiB, so
/// this is roughly ten times a realistic history rather than a tight fit.
pub const MAX_RAYCONFIG_CIPHERTEXT_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_PREPARED_SOURCE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_PREPARED_CACHE_BYTES: usize = 192 * 1024 * 1024;
pub const MAX_IMPORT_OPERATION_BYTES: usize = clipboard_store::MAX_IMPORT_OPERATION_BYTES;
pub const MAX_IMPORT_RUNTIME_BYTES: usize = 256 * 1024 * 1024;
pub const PREPARED_SESSION_CAPACITY: usize = 32;
pub const PREPARED_SESSION_TTL: Duration = Duration::from_secs(15 * 60);
const PREPARED_SESSION_OVERHEAD_BYTES: usize = 16 * 1024;
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
const _: () =
    assert!(MAX_PREPARED_RUNTIME_CONTROL_PROOF_BYTES <= MAX_PREPARED_RUNTIME_CONTROL_BYTES);

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
            max_sources <= PREPARED_SESSION_CAPACITY,
            "source admission count must fit the fixed session table"
        );
        assert!(
            source_control_bytes > 0 && source_control_bytes < max_source_bytes,
            "source control must be a strict carve-out of one source slot"
        );
        assert!(
            runtime_control_bytes >= MAX_PREPARED_RUNTIME_CONTROL_PROOF_BYTES,
            "runtime control must fit the fixed control proof"
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
    control: Arc<Mutex<RuntimeControl>>,
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
        let operation_gate = ImportOperationGate::process_wide();
        Self {
            admission_budget,
            operation_gate,
            control: Arc::new(Mutex::new(RuntimeControl::default())),
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
        self.control
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .sessions
            .evict_expired(Instant::now());
        loop {
            if let Some(reservation) = self.admission_budget.try_reserve()? {
                return Ok(reservation);
            }
            let evicted = self
                .control
                .lock()
                .map_err(|_| ImportError::service("analysis_unavailable"))?
                .sessions
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

    fn register_owner(&self) -> Result<ServiceOwnerToken, ImportError> {
        self.control
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .register_owner()
    }

    fn retain_owner(&self, owner: ServiceOwnerToken) {
        self.control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain_owner(owner);
    }

    fn release_owner(&self, owner: ServiceOwnerToken) {
        self.control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release_owner(owner);
    }

    #[cfg(test)]
    fn owner_refcount(&self, owner: ServiceOwnerToken) -> Option<usize> {
        self.control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owner_refcount(owner)
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
    /// Records deliberately left out: nothing was wrong with them, they simply
    /// pointed at a file that is no longer there.
    pub skipped: u64,
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
    fail_next_persistence: Option<Arc<AtomicBool>>,
    invalid_lease_offset_once: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    force_checkpoint_mismatch_once: Option<Arc<AtomicBool>>,
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
            fail_next_persistence: None,
            invalid_lease_offset_once: None,
            #[cfg(test)]
            force_checkpoint_mismatch_once: None,
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
            fail_next_persistence: None,
            invalid_lease_offset_once: None,
            #[cfg(test)]
            force_checkpoint_mismatch_once: None,
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
            fail_next_persistence: None,
            invalid_lease_offset_once: None,
            #[cfg(test)]
            force_checkpoint_mismatch_once: None,
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
            fail_next_persistence: None,
            invalid_lease_offset_once: None,
            #[cfg(test)]
            force_checkpoint_mismatch_once: None,
            #[cfg(test)]
            failure_hook: None,
        }
    }

    #[doc(hidden)]
    pub fn fail_next_persistence() -> Self {
        Self {
            fail_next_persistence: Some(Arc::new(AtomicBool::new(true))),
            ..Self::unbounded()
        }
    }

    #[doc(hidden)]
    pub fn invalid_lease_offset_once() -> Self {
        Self {
            invalid_lease_offset_once: Some(Arc::new(AtomicBool::new(true))),
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
            force_checkpoint_mismatch_once: Some(Arc::new(AtomicBool::new(true))),
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

    fn should_fail_next_persistence(&self) -> bool {
        take_policy_flag(&self.fail_next_persistence)
    }

    fn should_invalidate_lease_offset(&self) -> bool {
        take_policy_flag(&self.invalid_lease_offset_once)
    }

    #[cfg(test)]
    fn should_force_checkpoint_mismatch(&self) -> bool {
        take_policy_flag(&self.force_checkpoint_mismatch_once)
    }
}

fn take_policy_flag(flag: &Option<Arc<AtomicBool>>) -> bool {
    flag.as_ref()
        .is_some_and(|flag| flag.swap(false, Ordering::AcqRel))
}

impl Default for ImportWorkerPolicy {
    fn default() -> Self {
        Self::unbounded()
    }
}

pub struct ImportService {
    store: StoreHandle,
    worker_policy: ImportWorkerPolicy,
    runtime: ImportRuntime,
    owner: ServiceOwnerToken,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ServiceOwnerToken(Uuid);

impl ImportService {
    pub fn new(store: StoreHandle) -> Result<Self, ImportError> {
        Self::with_runtime_and_policy(
            store,
            ImportRuntime::process_wide(),
            ImportWorkerPolicy::default(),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_runtime(
        store: StoreHandle,
        runtime: ImportRuntime,
    ) -> Result<Self, ImportError> {
        Self::with_runtime_and_policy(store, runtime, ImportWorkerPolicy::default())
    }

    #[doc(hidden)]
    pub fn with_worker_policy(
        store: StoreHandle,
        worker_policy: ImportWorkerPolicy,
    ) -> Result<Self, ImportError> {
        Self::with_runtime_and_policy(store, ImportRuntime::process_wide(), worker_policy)
    }

    #[cfg(test)]
    pub(crate) fn with_worker_policy_and_limits(
        store: StoreHandle,
        worker_policy: ImportWorkerPolicy,
        admission_limits: ImportAdmissionLimits,
    ) -> Result<Self, ImportError> {
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
    ) -> Result<Self, ImportError> {
        let owner = runtime.register_owner()?;
        Ok(Self {
            store,
            worker_policy,
            runtime,
            owner,
        })
    }

    pub fn store(&self) -> &StoreHandle {
        &self.store
    }

    pub fn analyze(&self, path: impl AsRef<Path>) -> Result<ImportAnalysis, ImportError> {
        self.analyze_with_password(path, None)
    }

    /// Analyses an export, decrypting it first when it is a `.rayconfig`.
    ///
    /// The secret does not outlive this call. A prepared session keeps parsed
    /// records rather than a way back to the file, so starting the import it
    /// describes never needs the password again.
    pub fn analyze_with_password(
        &self,
        path: impl AsRef<Path>,
        secret: Option<&RayconfigSecret>,
    ) -> Result<ImportAnalysis, ImportError> {
        let source = self.prepare(path.as_ref(), secret)?;
        let total = source.source.total_records;
        let candidate_records = source.source.candidate_records();
        let failed = source.source.initial_failed_records();
        let skipped = source.source.initial_skipped_records();
        let analysis_id = Uuid::now_v7();
        let analysis_id = self
            .runtime
            .control
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .sessions
            .insert(self.owner, analysis_id, source, Instant::now())?;
        Ok(ImportAnalysis {
            analysis_id,
            total,
            candidate_records,
            skipped,
            failed,
        })
    }

    pub async fn begin(&self, analysis_id: Uuid) -> Result<ImportRunHandle, ImportError> {
        let action = self
            .runtime
            .control
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .sessions
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
            .control
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .sessions
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
        // A resumed rayconfig would need its password again, and the password
        // was deliberately not kept. Nothing ships a resume today; when
        // something does, the secret has to be threaded in here rather than
        // re-derived, because there is nothing to derive it from.
        let source = self.prepare_async(path.as_ref(), None).await?;
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
        self.run_to_completion_with_password(path, None).await
    }

    pub async fn run_to_completion_with_password(
        &self,
        path: impl AsRef<Path>,
        secret: Option<RayconfigSecret>,
    ) -> Result<ImportSummary, ImportError> {
        let source = self.prepare_async(path.as_ref(), secret).await?;
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
                initial_skips: source.skip_counts.clone(),
            })
            .await
            .map_err(map_store_error)
    }

    fn prepare(
        &self,
        path: &Path,
        secret: Option<&RayconfigSecret>,
    ) -> Result<PreparedSourceEnvelope, ImportError> {
        let operation_permit = self.runtime.acquire_operation_blocking()?;
        let reservation = self.runtime.reserve_preparation()?;
        self.prepare_with_admission(path, secret, operation_permit, reservation)
    }

    async fn prepare_async(
        &self,
        path: &Path,
        secret: Option<RayconfigSecret>,
    ) -> Result<PreparedSourceEnvelope, ImportError> {
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
            preparer.prepare_with_admission(&path, secret.as_ref(), operation_permit, reservation)
        })
        .await
        .map_err(|_| ImportError::service("analysis_unavailable"))?
    }

    fn prepare_with_admission(
        &self,
        path: &Path,
        secret: Option<&RayconfigSecret>,
        operation_permit: clipboard_store::ImportOperationPermit,
        mut reservation: AdmissionReservation,
    ) -> Result<PreparedSourceEnvelope, ImportError> {
        let limits = self.runtime.parse_limits();
        let source = prepare_source_with_permit(path, secret, &operation_permit, limits)?;
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
        let persisted = if self.worker_policy.should_fail_next_persistence() {
            self.store
                .inject_begin_import_failure()
                .await
                .map_err(map_store_error)
        } else {
            self.persist_run(analysis_id, &source.source).await
        };
        match persisted {
            Ok(mut lease) => {
                if self.worker_policy.should_invalidate_lease_offset() {
                    lease.next_candidate_offset = u64::MAX;
                }
                let handle = ImportRunHandle {
                    run_id: analysis_id,
                };
                if lease.state == StoreImportRunState::Running {
                    let offset = match usize::try_from(lease.next_candidate_offset) {
                        Ok(offset) if offset <= source.source.candidates.len() => offset,
                        _ => {
                            if let Ok(mut control) = self.runtime.control.lock() {
                                control.sessions.restore(
                                    self.owner,
                                    analysis_id,
                                    source,
                                    Instant::now(),
                                );
                            }
                            completion.finish(Err("invalid_run_state"));
                            return;
                        }
                    };
                    let marked_started =
                        self.runtime.control.lock().map(|mut control| {
                            control.sessions.mark_started(self.owner, analysis_id)
                        });
                    if !matches!(marked_started, Ok(true)) {
                        if let Ok(mut control) = self.runtime.control.lock() {
                            control.sessions.restore(
                                self.owner,
                                analysis_id,
                                source,
                                Instant::now(),
                            );
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
                        if let Ok(mut control) = worker.runtime.control.lock() {
                            control.sessions.finish_started(
                                worker.owner,
                                analysis_id,
                                completed,
                                Instant::now(),
                            );
                        }
                    });
                } else if let Ok(mut control) = self.runtime.control.lock() {
                    control.sessions.finish_completed(
                        self.owner,
                        analysis_id,
                        handle,
                        Instant::now(),
                    );
                }
                completion.finish(Ok(handle));
            }
            Err(error) => {
                let reason = import_error_reason(&error);
                if let Ok(mut control) = self.runtime.control.lock() {
                    control
                        .sessions
                        .restore(self.owner, analysis_id, source, Instant::now());
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
            if self.worker_policy.should_force_checkpoint_mismatch() {
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

impl Clone for ImportService {
    fn clone(&self) -> Self {
        self.runtime.retain_owner(self.owner);
        Self {
            store: self.store.clone(),
            worker_policy: self.worker_policy.clone(),
            runtime: self.runtime.clone(),
            owner: self.owner,
        }
    }
}

impl Drop for ImportService {
    fn drop(&mut self) {
        self.runtime.release_owner(self.owner);
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

struct ServiceOwnerRegistration {
    token: ServiceOwnerToken,
    refcount: usize,
}

struct RuntimeControl {
    owners: [Option<ServiceOwnerRegistration>; PREPARED_SESSION_CAPACITY],
    sessions: PreparedSessions,
}

impl Default for RuntimeControl {
    fn default() -> Self {
        Self {
            owners: std::array::from_fn(|_| None),
            sessions: PreparedSessions::default(),
        }
    }
}

impl RuntimeControl {
    fn register_owner(&mut self) -> Result<ServiceOwnerToken, ImportError> {
        let Some(index) = self.owners.iter().position(Option::is_none) else {
            return Err(ImportError::service("analysis_capacity_full"));
        };
        let token = ServiceOwnerToken(Uuid::now_v7());
        self.owners[index] = Some(ServiceOwnerRegistration { token, refcount: 1 });
        Ok(token)
    }

    fn retain_owner(&mut self, owner: ServiceOwnerToken) {
        let registration = self
            .owners
            .iter_mut()
            .flatten()
            .find(|registration| registration.token == owner)
            .expect("a live service clone has a registered owner");
        registration.refcount = registration
            .refcount
            .checked_add(1)
            .expect("service owner refcount must not overflow");
    }

    fn release_owner(&mut self, owner: ServiceOwnerToken) {
        let Some(index) = self.owners.iter().position(|registration| {
            registration
                .as_ref()
                .is_some_and(|item| item.token == owner)
        }) else {
            debug_assert!(false, "a dropped service must have a registered owner");
            return;
        };
        let registration = self.owners[index]
            .as_mut()
            .expect("the located owner slot is occupied");
        if registration.refcount > 1 {
            registration.refcount -= 1;
            return;
        }
        self.sessions.remove_owner(owner);
        self.owners[index] = None;
    }

    #[cfg(test)]
    fn owner_refcount(&self, owner: ServiceOwnerToken) -> Option<usize> {
        self.owners
            .iter()
            .flatten()
            .find_map(|registration| (registration.token == owner).then_some(registration.refcount))
    }
}

struct PreparedSessions {
    slots: [Option<PreparedSession>; PREPARED_SESSION_CAPACITY],
    ttl: Duration,
}

impl Default for PreparedSessions {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            ttl: PREPARED_SESSION_TTL,
        }
    }
}

impl PreparedSessions {
    #[cfg(test)]
    fn occupied_len(&self) -> usize {
        self.slots.iter().filter(|slot| slot.is_some()).count()
    }

    fn contains(&self, analysis_id: Uuid) -> bool {
        self.find_index(analysis_id).is_some()
    }

    fn find_index(&self, analysis_id: Uuid) -> Option<usize> {
        self.slots.iter().position(|slot| {
            slot.as_ref()
                .is_some_and(|session| session.analysis_id == analysis_id)
        })
    }

    fn remove_owner(&mut self, owner: ServiceOwnerToken) {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|session| session.owner == owner) {
                *slot = None;
            }
        }
    }

    fn insert(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        source: PreparedSourceEnvelope,
        now: Instant,
    ) -> Result<Uuid, ImportError> {
        self.evict_expired(now);
        if self.contains(analysis_id) {
            return Err(ImportError::service("analysis_capacity_full"));
        }
        let index = if let Some(index) = self.slots.iter().position(Option::is_none) {
            index
        } else if self.evict_oldest_available() {
            self.slots
                .iter()
                .position(Option::is_none)
                .expect("successful eviction leaves one fixed slot empty")
        } else {
            return Err(ImportError::service("analysis_capacity_full"));
        };
        self.slots[index] = Some(PreparedSession {
            analysis_id,
            owner,
            created_at: now,
            state: PreparedSessionState::Available {
                source,
                retired_completion: None,
            },
        });
        Ok(analysis_id)
    }

    fn begin(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        now: Instant,
    ) -> BeginPreparedSession {
        self.evict_expired(now);
        let Some(index) = self.find_index(analysis_id) else {
            return BeginPreparedSession::Missing;
        };
        if !self.slots[index]
            .as_ref()
            .is_some_and(|session| session.owner == owner)
        {
            return BeginPreparedSession::Missing;
        }
        let session = self.slots[index]
            .take()
            .expect("the located prepared-session slot is occupied");
        match session.state {
            PreparedSessionState::Available {
                source,
                retired_completion,
            } => {
                if let Some(completion) = retired_completion
                    && Arc::strong_count(&completion) > 1
                {
                    let waiter = completion.clone();
                    self.slots[index] = Some(PreparedSession {
                        analysis_id,
                        owner: session.owner,
                        created_at: session.created_at,
                        state: PreparedSessionState::Available {
                            source,
                            retired_completion: Some(completion),
                        },
                    });
                    return BeginPreparedSession::Wait(waiter);
                }
                let completion = Arc::new(PersistenceCompletion::default());
                self.slots[index] = Some(PreparedSession {
                    analysis_id,
                    owner: session.owner,
                    created_at: session.created_at,
                    state: PreparedSessionState::Persisting(completion.clone()),
                });
                BeginPreparedSession::Start { source, completion }
            }
            PreparedSessionState::Persisting(completion) => {
                let waiter = completion.clone();
                self.slots[index] = Some(PreparedSession {
                    analysis_id,
                    owner: session.owner,
                    created_at: session.created_at,
                    state: PreparedSessionState::Persisting(completion),
                });
                BeginPreparedSession::Wait(waiter)
            }
            PreparedSessionState::Started(completion) => {
                let waiter = completion.clone();
                self.slots[index] = Some(PreparedSession {
                    analysis_id,
                    owner: session.owner,
                    created_at: session.created_at,
                    state: PreparedSessionState::Started(completion),
                });
                BeginPreparedSession::Wait(waiter)
            }
            PreparedSessionState::Completed { handle, completion } => {
                self.slots[index] = Some(PreparedSession {
                    analysis_id,
                    owner: session.owner,
                    created_at: session.created_at,
                    state: PreparedSessionState::Completed { handle, completion },
                });
                BeginPreparedSession::Ready(handle)
            }
            PreparedSessionState::Consumed(completion) => {
                self.slots[index] = Some(PreparedSession {
                    analysis_id,
                    owner: session.owner,
                    created_at: session.created_at,
                    state: PreparedSessionState::Consumed(completion),
                });
                BeginPreparedSession::Missing
            }
        }
    }

    fn mark_started(&mut self, owner: ServiceOwnerToken, analysis_id: Uuid) -> bool {
        let Some(index) = self.find_index(analysis_id) else {
            return false;
        };
        if !self.slots[index]
            .as_ref()
            .is_some_and(|session| session.owner == owner)
        {
            return false;
        }
        let session = self.slots[index]
            .take()
            .expect("the located prepared-session slot is occupied");
        let PreparedSessionState::Persisting(completion) = session.state else {
            self.slots[index] = Some(session);
            return false;
        };
        self.slots[index] = Some(PreparedSession {
            analysis_id,
            owner: session.owner,
            created_at: session.created_at,
            state: PreparedSessionState::Started(completion),
        });
        true
    }

    fn restore(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        source: PreparedSourceEnvelope,
        now: Instant,
    ) -> bool {
        let Some(index) = self.find_index(analysis_id) else {
            return false;
        };
        if !self.slots[index]
            .as_ref()
            .is_some_and(|session| session.owner == owner)
        {
            return false;
        }
        let session = self.slots[index]
            .take()
            .expect("the located prepared-session slot is occupied");
        let retired_completion = match session.state {
            PreparedSessionState::Available {
                retired_completion, ..
            } => retired_completion,
            PreparedSessionState::Persisting(completion)
            | PreparedSessionState::Started(completion)
            | PreparedSessionState::Consumed(completion)
            | PreparedSessionState::Completed { completion, .. } => Some(completion),
        };
        self.slots[index] = Some(PreparedSession {
            analysis_id,
            owner,
            created_at: now,
            state: PreparedSessionState::Available {
                source,
                retired_completion,
            },
        });
        true
    }

    fn finish_persisting(&mut self, analysis_id: Uuid) {
        if let Some(index) = self.find_index(analysis_id) {
            self.slots[index] = None;
        }
    }

    fn finish_started(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        completed: bool,
        now: Instant,
    ) {
        let Some(index) = self.find_index(analysis_id) else {
            return;
        };
        if !self.slots[index]
            .as_ref()
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
            let session = self.slots[index]
                .take()
                .expect("the located prepared-session slot is occupied");
            let completion = match session.state {
                PreparedSessionState::Started(completion)
                | PreparedSessionState::Persisting(completion) => completion,
                state => {
                    self.slots[index] = Some(PreparedSession { state, ..session });
                    return;
                }
            };
            self.slots[index] = Some(PreparedSession {
                analysis_id,
                owner,
                created_at: now,
                state: PreparedSessionState::Consumed(completion),
            });
        }
    }

    fn finish_completed(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        handle: ImportRunHandle,
        now: Instant,
    ) {
        let Some(index) = self.find_index(analysis_id) else {
            return;
        };
        if self.slots[index]
            .as_ref()
            .is_some_and(|session| session.owner == owner)
        {
            let session = self.slots[index]
                .take()
                .expect("the located prepared-session slot is occupied");
            let completion = match session.state {
                PreparedSessionState::Persisting(completion)
                | PreparedSessionState::Started(completion)
                | PreparedSessionState::Consumed(completion)
                | PreparedSessionState::Completed { completion, .. } => completion,
                state @ PreparedSessionState::Available { .. } => {
                    self.slots[index] = Some(PreparedSession { state, ..session });
                    return;
                }
            };
            self.slots[index] = Some(PreparedSession {
                analysis_id,
                owner,
                created_at: now,
                state: PreparedSessionState::Completed { handle, completion },
            });
        }
    }

    fn discard(
        &mut self,
        owner: ServiceOwnerToken,
        analysis_id: Uuid,
        now: Instant,
    ) -> Result<(), ImportError> {
        self.evict_expired(now);
        let Some(index) = self.find_index(analysis_id) else {
            return Err(ImportError::service("analysis_not_found"));
        };
        if !self.slots[index]
            .as_ref()
            .is_some_and(|session| session.owner == owner)
        {
            return Err(ImportError::service("analysis_not_found"));
        }
        match self.slots[index].as_ref().map(|session| &session.state) {
            Some(PreparedSessionState::Available {
                retired_completion, ..
            }) => {
                if retired_completion
                    .as_ref()
                    .is_some_and(|completion| Arc::strong_count(completion) > 1)
                {
                    Err(ImportError::service("analysis_in_progress"))
                } else {
                    self.finish_persisting(analysis_id);
                    Ok(())
                }
            }
            Some(PreparedSessionState::Persisting(_)) => {
                Err(ImportError::service("analysis_in_progress"))
            }
            Some(PreparedSessionState::Started(_)) => {
                Err(ImportError::service("analysis_in_progress"))
            }
            Some(PreparedSessionState::Completed { completion, .. }) => {
                if Arc::strong_count(completion) > 1 {
                    Err(ImportError::service("analysis_in_progress"))
                } else {
                    self.finish_persisting(analysis_id);
                    Ok(())
                }
            }
            Some(PreparedSessionState::Consumed(_)) => {
                Err(ImportError::service("analysis_not_found"))
            }
            None => Err(ImportError::service("analysis_not_found")),
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        for slot in &mut self.slots {
            let expired = slot.as_ref().is_some_and(|session| {
                session_is_evictable(session)
                    && now.saturating_duration_since(session.created_at) >= self.ttl
            });
            if expired {
                *slot = None;
            }
        }
    }

    fn evict_oldest_available(&mut self) -> bool {
        let oldest = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(index, slot)| {
                slot.as_ref().and_then(|session| {
                    session_is_evictable(session).then_some((session.created_at, index))
                })
            })
            .min_by_key(|(created_at, index)| (*created_at, *index))
            .map(|(_, index)| index);
        if let Some(index) = oldest {
            self.slots[index] = None;
            true
        } else {
            false
        }
    }
}

fn session_is_evictable(session: &PreparedSession) -> bool {
    match &session.state {
        PreparedSessionState::Available {
            retired_completion, ..
        } => retired_completion
            .as_ref()
            .is_none_or(|completion| Arc::strong_count(completion) == 1),
        PreparedSessionState::Completed { completion, .. }
        | PreparedSessionState::Consumed(completion) => Arc::strong_count(completion) == 1,
        PreparedSessionState::Persisting(_) | PreparedSessionState::Started(_) => false,
    }
}

struct PreparedSession {
    analysis_id: Uuid,
    owner: ServiceOwnerToken,
    created_at: Instant,
    state: PreparedSessionState,
}

enum PreparedSessionState {
    Available {
        source: PreparedSourceEnvelope,
        retired_completion: Option<Arc<PersistenceCompletion>>,
    },
    Persisting(Arc<PersistenceCompletion>),
    Started(Arc<PersistenceCompletion>),
    Completed {
        handle: ImportRunHandle,
        completion: Arc<PersistenceCompletion>,
    },
    Consumed(Arc<PersistenceCompletion>),
}

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

const fn arc_allocation_upper_bound<T>() -> usize {
    2 * size_of::<usize>() + (align_of::<T>() - 1) + size_of::<T>()
}

const MAX_PERSISTENCE_COMPLETION_ALLOCATIONS: usize = PREPARED_SESSION_CAPACITY;
const MAX_PREPARED_RUNTIME_CONTROL_PROOF_BYTES: usize =
    arc_allocation_upper_bound::<Mutex<RuntimeControl>>()
        + arc_allocation_upper_bound::<Mutex<AdmissionLedger>>()
        + MAX_PERSISTENCE_COMPLETION_ALLOCATIONS
            * arc_allocation_upper_bound::<PersistenceCompletion>()
        + clipboard_store::IMPORT_OPERATION_GATE_CONTROL_BYTES
        + size_of::<OnceLock<ImportRuntime>>()
        + size_of::<OnceLock<ImportOperationGate>>()
        + MAX_IMPORT_HANDOFF_CONTROL_BYTES;

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
    skip_counts: Vec<ImportFailureCount>,
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

    fn initial_skipped_records(&self) -> u64 {
        self.skip_counts.iter().map(|skip| skip.count).sum()
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
            )
            .saturating_add(
                self.skip_counts
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
        for failure in self.failure_counts.iter().chain(&self.skip_counts) {
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
    secret: Option<&RayconfigSecret>,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
) -> Result<PreparedSource, ImportError> {
    prepare_source_with_hook(path, secret, permit, limits, || {})
}

fn prepare_source_with_hook(
    path: &Path,
    secret: Option<&RayconfigSecret>,
    permit: &clipboard_store::ImportOperationPermit,
    limits: ImportParseLimits,
    after_initial_detection: impl FnOnce(),
) -> Result<PreparedSource, ImportError> {
    let detected = detect_export_with_permit(path, permit, limits)?;
    after_initial_detection();
    let report = parse_detected_export_report_with_permit(&detected, secret, permit, limits);
    let verified = detect_export_with_permit(path, permit, limits);
    let source_is_unchanged = verified.as_ref().is_ok_and(|verified| {
        verified.source == detected.source
            && verified.source_fingerprint == detected.source_fingerprint
    });
    if !source_is_unchanged {
        return Err(ImportError::service("source_changed"));
    }
    let mut report = report?;
    drop_unreachable_candidates(&mut report)?;
    let total_records =
        u64::try_from(report.total).map_err(|_| ImportError::service("source_too_large"))?;
    let source_fingerprint = prepared_snapshot_fingerprint(
        detected.source,
        detected.source_fingerprint,
        total_records,
        &report,
    )?;
    let failure_counts = aggregate_reasons(&report, &report.failures, limits.source_control_bytes)?;
    let skip_counts = aggregate_reasons(&report, &report.skips, limits.source_control_bytes)?;
    Ok(PreparedSource {
        source_kind: store_source(detected.source),
        source_fingerprint,
        total_records,
        candidates: report.candidates,
        failure_counts,
        skip_counts,
    })
}

/// Moves candidates that lead nowhere from the import into the skip bucket.
///
/// Policy, not parsing: the parsers still map every source record faithfully so
/// their own guarantees stay testable, and the decision to leave a record out
/// is taken once, here, where the whole report is in hand.
fn drop_unreachable_candidates(report: &mut ImportParseReport) -> Result<(), ImportError> {
    let mut retained = Vec::with_capacity(report.candidates.len());
    for candidate in std::mem::take(&mut report.candidates) {
        if crate::candidate_leads_nowhere(&candidate) {
            let skip =
                crate::record_failure(candidate.source, candidate.source_record, "source_missing");
            report.push_skip(skip)?;
            continue;
        }
        retained.push(candidate);
    }
    report.candidates = retained;
    Ok(())
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
    // Skips belong in the snapshot too: a resume that reparses the same source
    // on a machine where a referenced file has since disappeared produces a
    // different set, and that must be detected rather than silently accepted.
    hasher.add_optional(Some(&(report.skips.len() as u64).to_be_bytes()));
    for skip in &report.skips {
        let record =
            u64::try_from(skip.record).map_err(|_| ImportError::service("source_too_large"))?;
        hasher.add_optional(Some(b"skip"));
        hasher.add_optional(Some(&record.to_be_bytes()));
        hasher.add_optional(Some(skip.reason.as_bytes()));
    }
    Ok(hasher.finish())
}

/// Collapses a bucket of per-record reasons into bounded counts.
///
/// Used for both failures and deliberate skips: the shapes are identical and
/// the same fixed reason-slot budget applies to each.
fn aggregate_reasons(
    report: &ImportParseReport,
    entries: &[crate::ImportRecordFailure],
    source_control_bytes: usize,
) -> Result<Vec<ImportFailureCount>, ImportError> {
    aggregate_reasons_with_hook(report, entries, source_control_bytes, |_, _| {})
}

fn aggregate_reasons_with_hook(
    report: &ImportParseReport,
    entries: &[crate::ImportRecordFailure],
    source_control_bytes: usize,
    before_allocation: impl FnOnce(usize, usize),
) -> Result<Vec<ImportFailureCount>, ImportError> {
    let _ = report;
    let mut counts = [("", 0_u64); MAX_IMPORT_FAILURE_KINDS];
    let mut used = 0_usize;
    for failure in entries {
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

    #[test]
    fn default_worker_policy_has_no_heap_allocation() {
        let mut policy = None;

        let allocations = allocation_counter::measure(|| {
            policy = Some(ImportWorkerPolicy::default());
        });

        assert!(policy.is_some());
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    }

    #[test]
    #[should_panic(expected = "source admission count must fit the fixed session table")]
    fn injected_source_count_cannot_exceed_the_fixed_session_table() {
        let _ = ImportAdmissionLimits::new(
            PREPARED_SESSION_CAPACITY + 1,
            MAX_PREPARED_SOURCE_BYTES,
            MAX_PREPARED_SOURCE_CONTROL_BYTES,
            MAX_PREPARED_CACHE_BYTES,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_RUNTIME_BYTES,
        );
    }

    #[tokio::test]
    async fn service_owner_registry_rejects_the_thirty_third_owner_without_allocation() {
        let _test_guard = runtime_memory_test_guard().await;
        let database = tempfile::tempdir().unwrap();
        let store = StoreHandle::open(StoreConfig::new(
            database.path().join("synthetic-owner-registry.sqlite"),
        ))
        .unwrap();
        let mut services = Vec::with_capacity(PREPARED_SESSION_CAPACITY);
        for _ in 0..PREPARED_SESSION_CAPACITY {
            services.push(ImportService::new(store.clone()).unwrap());
        }
        let rejected_store = store.clone();
        let mut rejected = None;

        let allocations = allocation_counter::measure(|| {
            rejected = Some(ImportService::new(rejected_store));
        });

        assert!(matches!(
            rejected,
            Some(Err(ImportError::Service {
                reason: "analysis_capacity_full"
            }))
        ));
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
        drop(services);
    }

    #[test]
    fn service_clone_and_drop_update_the_fixed_owner_refcount() {
        let database = tempfile::tempdir().unwrap();
        let runtime = ImportRuntime::with_limits(ImportAdmissionLimits::default());
        let service = ImportService::with_runtime(
            StoreHandle::open(StoreConfig::new(
                database.path().join("synthetic-owner-refcount.sqlite"),
            ))
            .unwrap(),
            runtime.clone(),
        )
        .unwrap();
        let owner = service.owner;
        let analysis_id = Uuid::now_v7();
        {
            let mut control = runtime.control.lock().unwrap();
            let source = admitted_source(
                &mut control.sessions,
                &runtime.admission_budget,
                empty_source(1),
            );
            control
                .sessions
                .insert(owner, analysis_id, source, Instant::now())
                .unwrap();
        }

        assert_eq!(runtime.owner_refcount(owner), Some(1));
        let cloned = service.clone();
        assert_eq!(runtime.owner_refcount(owner), Some(2));
        drop(service);
        assert_eq!(runtime.owner_refcount(owner), Some(1));
        assert!(
            runtime
                .control
                .lock()
                .unwrap()
                .sessions
                .contains(analysis_id)
        );
        drop(cloned);
        assert_eq!(runtime.owner_refcount(owner), None);
        assert!(
            !runtime
                .control
                .lock()
                .unwrap()
                .sessions
                .contains(analysis_id)
        );
        assert_eq!(runtime.snapshot().0, runtime.limits.runtime_control_bytes);
    }

    #[test]
    fn fixed_session_table_rejects_a_thirty_third_non_evictable_entry_without_allocation() {
        let mut sessions = PreparedSessions::default();
        let budget = AdmissionBudget::new(ImportAdmissionLimits::default());
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let now = Instant::now();
        for index in 0..PREPARED_SESSION_CAPACITY {
            let analysis_id = Uuid::now_v7();
            let source = admitted_source(
                &mut sessions,
                &budget,
                empty_source(u8::try_from(index).unwrap()),
            );
            sessions.insert(owner, analysis_id, source, now).unwrap();
            match sessions.begin(owner, analysis_id, now) {
                BeginPreparedSession::Start { source, .. } => drop(source),
                _ => panic!("new session must transition to non-evictable persistence"),
            }
        }
        assert_eq!(sessions.occupied_len(), PREPARED_SESSION_CAPACITY);
        let rejected_source = admitted_source(&mut sessions, &budget, empty_source(255));
        let rejected_id = Uuid::now_v7();
        let mut rejected = None;

        let allocations = allocation_counter::measure(|| {
            rejected = Some(sessions.insert(owner, rejected_id, rejected_source, now));
        });

        assert!(matches!(
            rejected,
            Some(Err(ImportError::Service {
                reason: "analysis_capacity_full"
            }))
        ));
        assert_eq!(sessions.occupied_len(), PREPARED_SESSION_CAPACITY);
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
    }

    #[test]
    fn completed_observers_keep_their_fixed_slots_until_completion_is_unshared() {
        let mut sessions = PreparedSessions::default();
        let budget = AdmissionBudget::new(ImportAdmissionLimits::default());
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let now = Instant::now();
        let mut observed_completions = Vec::with_capacity(PREPARED_SESSION_CAPACITY);
        for index in 0..PREPARED_SESSION_CAPACITY {
            let analysis_id = Uuid::now_v7();
            let source = admitted_source(
                &mut sessions,
                &budget,
                empty_source(u8::try_from(index).unwrap()),
            );
            sessions.insert(owner, analysis_id, source, now).unwrap();
            let BeginPreparedSession::Start { source, completion } =
                sessions.begin(owner, analysis_id, now)
            else {
                panic!("new session must start persistence");
            };
            drop(source);
            assert!(sessions.mark_started(owner, analysis_id));
            sessions.finish_started(owner, analysis_id, true, now);
            observed_completions.push(completion);
        }

        let rejected_source = admitted_source(&mut sessions, &budget, empty_source(254));
        assert!(matches!(
            sessions.insert(owner, Uuid::now_v7(), rejected_source, now),
            Err(ImportError::Service {
                reason: "analysis_capacity_full"
            })
        ));
        assert_eq!(sessions.occupied_len(), PREPARED_SESSION_CAPACITY);

        drop(observed_completions);
        let admitted = admitted_source(&mut sessions, &budget, empty_source(255));
        assert!(
            sessions
                .insert(owner, Uuid::now_v7(), admitted, now)
                .is_ok()
        );
        assert_eq!(sessions.occupied_len(), PREPARED_SESSION_CAPACITY);
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

        let failures = aggregate_reasons_with_hook(
            &report,
            &report.failures,
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
    fn fixed_runtime_control_layout_is_allocation_free_and_fits_the_baseline() {
        let mut control = None;

        let allocations = allocation_counter::measure(|| {
            control = Some(RuntimeControl::default());
        });

        assert!(control.is_some());
        assert_eq!(allocations.count_total, 0, "{allocations:?}");
        assert_eq!(allocations.bytes_total, 0, "{allocations:?}");
        let mut completions: [Option<Arc<PersistenceCompletion>>; PREPARED_SESSION_CAPACITY] =
            std::array::from_fn(|_| None);
        let completion_allocations = allocation_counter::measure(|| {
            for slot in &mut completions {
                *slot = Some(Arc::new(PersistenceCompletion::default()));
            }
        });
        assert_eq!(
            completion_allocations.count_total, PREPARED_SESSION_CAPACITY as u64,
            "{completion_allocations:?}"
        );
        assert!(
            completion_allocations.bytes_total
                <= (PREPARED_SESSION_CAPACITY
                    * arc_allocation_upper_bound::<PersistenceCompletion>())
                    as u64,
            "{completion_allocations:?}"
        );
        assert_eq!(
            MAX_PERSISTENCE_COMPLETION_ALLOCATIONS,
            PREPARED_SESSION_CAPACITY
        );
        let proof_bytes = std::hint::black_box(MAX_PREPARED_RUNTIME_CONTROL_PROOF_BYTES);
        assert!(proof_bytes > size_of::<RuntimeControl>());
        assert!(proof_bytes <= MAX_PREPARED_RUNTIME_CONTROL_BYTES);
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
            128 * 1024,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 128 * 1024,
        ));
        let first = ImportService::with_runtime(
            StoreHandle::open(StoreConfig::new(
                first_database.path().join("history.sqlite"),
            ))
            .unwrap(),
            runtime.clone(),
        )
        .unwrap();
        let second = ImportService::with_runtime(
            StoreHandle::open(StoreConfig::new(
                second_database.path().join("history.sqlite"),
            ))
            .unwrap(),
            runtime.clone(),
        )
        .unwrap();

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
            192 * 1024,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 192 * 1024,
        ));
        let store =
            StoreHandle::open(StoreConfig::new(database.path().join("history.sqlite"))).unwrap();
        let owner = ImportService::with_runtime(store.clone(), runtime.clone()).unwrap();
        let stranger = ImportService::with_runtime(store, runtime.clone()).unwrap();
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
            192 * 1024,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 192 * 1024,
        );
        let service = ImportService::with_worker_policy_and_limits(
            StoreHandle::open(StoreConfig::new(database.path().join("history.sqlite"))).unwrap(),
            ImportWorkerPolicy::fail_next_persistence(),
            limits,
        )
        .unwrap();
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
            192 * 1024,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            MAX_IMPORT_OPERATION_BYTES,
            MAX_IMPORT_OPERATION_BYTES + 192 * 1024,
        ));
        let service = ImportService::with_runtime(
            StoreHandle::open(StoreConfig::new(database.path().join("history.sqlite"))).unwrap(),
            runtime.clone(),
        )
        .unwrap();
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
        let service = ImportService::new(store).unwrap();
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
        let service = ImportService::new(store.clone()).unwrap();
        let export = synthetic_export();

        make_blob_storage_private(&data_dir);
        let persistence_error = service.run_to_completion(export.path()).await.unwrap_err();
        assert_private_service_error(persistence_error);
        restore_blob_storage(&data_dir);

        let resumable = service.prepare(export.path(), None).unwrap();
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
        )
        .unwrap();
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
        )
        .unwrap();
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
        let result = prepare_source_with_hook(
            export.path(),
            None,
            &permit,
            ImportParseLimits::default(),
            || {
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
            },
        );

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
            MAX_PREPARED_RUNTIME_CONTROL_BYTES + PREPARED_SESSION_CAPACITY * 32 * 1024,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            4 * 1024,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES + PREPARED_SESSION_CAPACITY * 32 * 1024 + 4 * 1024,
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
            sessions
                .insert(
                    owner,
                    analysis_id,
                    envelope,
                    now + Duration::from_millis(index as u64),
                )
                .unwrap();
            first.get_or_insert(analysis_id);
            newest = Some(analysis_id);
        }

        assert!(!sessions.contains(first.unwrap()));
        assert!(sessions.contains(newest.unwrap()));
        assert_eq!(sessions.occupied_len(), PREPARED_SESSION_CAPACITY);
    }

    #[test]
    fn prepared_session_cache_expires_available_entries_at_the_fixed_ttl() {
        let mut sessions = PreparedSessions::default();
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            1,
            32 * 1024,
            8 * 1024,
            96 * 1024,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            4 * 1024,
            100 * 1024,
        ));
        let now = Instant::now();
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let analysis_id = Uuid::now_v7();
        let source = admitted_source(&mut sessions, &budget, empty_source(1));
        sessions.insert(owner, analysis_id, source, now).unwrap();

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
            MAX_PREPARED_RUNTIME_CONTROL_BYTES
                .saturating_add(retained_bytes.saturating_mul(2))
                .saturating_sub(1),
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            retained_bytes,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES
                .saturating_add(retained_bytes.saturating_mul(3))
                .saturating_sub(1),
        ));
        let mut sessions = PreparedSessions::default();
        let now = Instant::now();
        let owner = ServiceOwnerToken(Uuid::now_v7());
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let first_source = admitted_source(&mut sessions, &budget, source);
        sessions.insert(owner, first, first_source, now).unwrap();

        let second_source = admitted_source(&mut sessions, &budget, empty_source(2));
        sessions
            .insert(owner, second, second_source, now + Duration::from_millis(1))
            .unwrap();

        assert!(!sessions.contains(first));
        assert!(sessions.contains(second));
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
            MAX_PREPARED_RUNTIME_CONTROL_BYTES.saturating_add(retained_bytes.saturating_mul(2)),
            MAX_PREPARED_RUNTIME_CONTROL_BYTES,
            retained_bytes,
            MAX_PREPARED_RUNTIME_CONTROL_BYTES.saturating_add(retained_bytes.saturating_mul(3)),
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
            skip_counts: Vec::new(),
        }
    }

    fn source_with_payload(payload_bytes: usize) -> PreparedSource {
        PreparedSource {
            source_kind: ImportSourceKind::SuperCmd,
            source_fingerprint: [3; 32],
            total_records: 1,
            candidates: vec![ImportCandidate {
                source: ImportSource::SuperCmd,
                source_record: 1,
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
                    display_label: None,
                },
                search_ocr: None,
                missing_payload: false,
                source_application_path: None,
            }],
            failure_counts: Vec::new(),
            skip_counts: Vec::new(),
        }
    }
}
