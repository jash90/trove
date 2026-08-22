use std::{
    collections::{BTreeMap, VecDeque},
    mem::size_of,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
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
pub const MAX_IMPORT_MANIFEST_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMPORT_AUXILIARY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_PREPARED_SOURCE_BYTES: usize = 128 * 1024 * 1024;
pub const MAX_PREPARED_CACHE_BYTES: usize = 256 * 1024 * 1024;
pub const PREPARED_SESSION_CAPACITY: usize = 32;
pub const PREPARED_SESSION_TTL: Duration = Duration::from_secs(15 * 60);
const PREPARED_SESSION_OVERHEAD_BYTES: usize = 1024;

/// Hard admission limits shared by analyzed sessions, resume preparations, CLI imports, and
/// active workers. Production reserves one full per-source allowance before every parse so the
/// number and aggregate memory envelope remain finite even while parsing.
#[derive(Clone, Copy)]
pub struct ImportAdmissionLimits {
    max_sources: usize,
    max_source_bytes: usize,
    total_bytes: usize,
}

impl ImportAdmissionLimits {
    #[doc(hidden)]
    pub const fn new(max_sources: usize, max_source_bytes: usize, total_bytes: usize) -> Self {
        assert!(max_sources > 0, "source admission count must be positive");
        assert!(
            max_source_bytes > 0,
            "source admission size must be positive"
        );
        assert!(
            total_bytes >= max_source_bytes,
            "aggregate admission size must fit one source"
        );
        Self {
            max_sources,
            max_source_bytes,
            total_bytes,
        }
    }
}

impl Default for ImportAdmissionLimits {
    fn default() -> Self {
        Self::new(
            PREPARED_SESSION_CAPACITY,
            MAX_PREPARED_SOURCE_BYTES,
            MAX_PREPARED_CACHE_BYTES,
        )
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
    admission_budget: AdmissionBudget,
    analysis_gate: Arc<Mutex<()>>,
}

impl ImportService {
    pub fn new(store: StoreHandle) -> Self {
        Self::with_worker_policy(store, ImportWorkerPolicy::default())
    }

    #[doc(hidden)]
    pub fn with_worker_policy(store: StoreHandle, worker_policy: ImportWorkerPolicy) -> Self {
        Self::with_worker_policy_and_limits(store, worker_policy, ImportAdmissionLimits::default())
    }

    #[doc(hidden)]
    pub fn with_worker_policy_and_limits(
        store: StoreHandle,
        worker_policy: ImportWorkerPolicy,
        admission_limits: ImportAdmissionLimits,
    ) -> Self {
        Self {
            store,
            worker_policy: Arc::new(worker_policy),
            prepared_sessions: Arc::new(Mutex::new(PreparedSessions::default())),
            admission_budget: AdmissionBudget::new(admission_limits),
            analysis_gate: Arc::new(Mutex::new(())),
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
            .prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .insert(analysis_id, source, Instant::now());
        Ok(ImportAnalysis {
            analysis_id,
            total,
            candidate_records,
            failed,
        })
    }

    pub async fn begin(&self, analysis_id: Uuid) -> Result<ImportRunHandle, ImportError> {
        let action = self
            .prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .begin(analysis_id, Instant::now());
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
            BeginPreparedSession::Missing => {
                return match self.status(analysis_id) {
                    Ok(_) => Ok(ImportRunHandle {
                        run_id: analysis_id,
                    }),
                    Err(ImportError::Service {
                        reason: "run_not_found",
                    }) => Err(ImportError::service("analysis_not_found")),
                    Err(error) => Err(error),
                };
            }
        };
        completion.wait().await
    }

    pub fn discard_analysis(&self, analysis_id: Uuid) -> Result<(), ImportError> {
        self.prepared_sessions
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?
            .discard(analysis_id, Instant::now())
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
        let source = self.prepare(path.as_ref())?;
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
        let source = self.prepare(path.as_ref())?;
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
        let _analysis_permit = self
            .analysis_gate
            .lock()
            .map_err(|_| ImportError::service("analysis_unavailable"))?;
        let mut reservation = self.reserve_preparation()?;
        let source = prepare_source(path)?;
        reservation.resize(source.retained_bytes())?;
        Ok(PreparedSourceEnvelope {
            source,
            _reservation: reservation,
        })
    }

    fn reserve_preparation(&self) -> Result<AdmissionReservation, ImportError> {
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
                            if let Ok(mut sessions) = self.prepared_sessions.lock() {
                                sessions.restore(analysis_id, source, Instant::now());
                            }
                            completion.finish(Err("invalid_run_state"));
                            return;
                        }
                    };
                    let marked_started = self
                        .prepared_sessions
                        .lock()
                        .map(|mut sessions| sessions.mark_started(analysis_id));
                    if !matches!(marked_started, Ok(true)) {
                        if let Ok(mut sessions) = self.prepared_sessions.lock() {
                            sessions.restore(analysis_id, source, Instant::now());
                        }
                        completion.finish(Err("analysis_unavailable"));
                        return;
                    }
                    let worker = self.clone();
                    tokio::spawn(async move {
                        let _ = worker
                            .run_worker(analysis_id, lease.generation, source, offset)
                            .await;
                    });
                    if let Ok(mut sessions) = self.prepared_sessions.lock() {
                        sessions.finish_started(analysis_id);
                    }
                } else if let Ok(mut sessions) = self.prepared_sessions.lock() {
                    sessions.finish_persisting(analysis_id);
                }
                completion.finish(Ok(handle));
            }
            Err(error) => {
                let reason = import_error_reason(&error);
                if let Ok(mut sessions) = self.prepared_sessions.lock() {
                    sessions.restore(analysis_id, source, Instant::now());
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
        let mut remaining = source
            .candidates
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

#[derive(Clone)]
struct AdmissionBudget {
    ledger: Arc<Mutex<AdmissionLedger>>,
    limits: ImportAdmissionLimits,
}

impl AdmissionBudget {
    fn new(limits: ImportAdmissionLimits) -> Self {
        Self {
            ledger: Arc::new(Mutex::new(AdmissionLedger::default())),
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
            || retained_bytes > self.limits.total_bytes
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

#[derive(Default)]
struct AdmissionLedger {
    source_count: usize,
    retained_bytes: usize,
}

struct AdmissionReservation {
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
        if resized_total > self.limits.total_bytes {
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
}

impl Default for PreparedSessions {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            insertion_order: VecDeque::new(),
            ttl: PREPARED_SESSION_TTL,
        }
    }
}

impl PreparedSessions {
    fn insert(&mut self, analysis_id: Uuid, source: PreparedSourceEnvelope, now: Instant) -> Uuid {
        self.evict_expired(now);
        self.sessions.insert(
            analysis_id,
            PreparedSession {
                created_at: now,
                state: PreparedSessionState::Available(source),
            },
        );
        self.insertion_order.push_back(analysis_id);
        analysis_id
    }

    fn begin(&mut self, analysis_id: Uuid, now: Instant) -> BeginPreparedSession {
        self.evict_expired(now);
        let Some(session) = self.sessions.remove(&analysis_id) else {
            return BeginPreparedSession::Missing;
        };
        match session.state {
            PreparedSessionState::Available(source) => {
                self.insertion_order
                    .retain(|candidate| *candidate != analysis_id);
                let completion = Arc::new(PersistenceCompletion::default());
                self.sessions.insert(
                    analysis_id,
                    PreparedSession {
                        created_at: session.created_at,
                        state: PreparedSessionState::Persisting(completion.clone()),
                    },
                );
                BeginPreparedSession::Start { source, completion }
            }
            PreparedSessionState::Persisting(completion) => {
                self.sessions.insert(
                    analysis_id,
                    PreparedSession {
                        created_at: session.created_at,
                        state: PreparedSessionState::Persisting(completion.clone()),
                    },
                );
                BeginPreparedSession::Wait(completion)
            }
            PreparedSessionState::Started(completion) => {
                self.sessions.insert(
                    analysis_id,
                    PreparedSession {
                        created_at: session.created_at,
                        state: PreparedSessionState::Started(completion.clone()),
                    },
                );
                BeginPreparedSession::Wait(completion)
            }
        }
    }

    fn mark_started(&mut self, analysis_id: Uuid) -> bool {
        let Some(session) = self.sessions.remove(&analysis_id) else {
            return false;
        };
        let PreparedSessionState::Persisting(completion) = session.state else {
            self.sessions.insert(analysis_id, session);
            return false;
        };
        self.sessions.insert(
            analysis_id,
            PreparedSession {
                created_at: session.created_at,
                state: PreparedSessionState::Started(completion),
            },
        );
        true
    }

    fn restore(&mut self, analysis_id: Uuid, source: PreparedSourceEnvelope, now: Instant) {
        self.sessions.remove(&analysis_id);
        self.sessions.insert(
            analysis_id,
            PreparedSession {
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

    fn finish_started(&mut self, analysis_id: Uuid) {
        self.finish_persisting(analysis_id);
    }

    fn discard(&mut self, analysis_id: Uuid, now: Instant) -> Result<(), ImportError> {
        self.evict_expired(now);
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
            None => Err(ImportError::service("analysis_not_found")),
        }
    }

    fn evict_expired(&mut self, now: Instant) {
        let expired = self
            .insertion_order
            .iter()
            .copied()
            .filter(|analysis_id| {
                self.sessions.get(analysis_id).is_some_and(|session| {
                    matches!(session.state, PreparedSessionState::Available(_))
                        && now.saturating_duration_since(session.created_at) >= self.ttl
                })
            })
            .collect::<Vec<_>>();
        for analysis_id in expired {
            self.finish_persisting(analysis_id);
        }
    }

    fn evict_oldest_available(&mut self) -> bool {
        while let Some(analysis_id) = self.insertion_order.pop_front() {
            if self
                .sessions
                .get(&analysis_id)
                .is_some_and(|session| matches!(session.state, PreparedSessionState::Available(_)))
            {
                self.finish_persisting(analysis_id);
                return true;
            }
        }
        false
    }
}

struct PreparedSession {
    created_at: Instant,
    state: PreparedSessionState,
}

enum PreparedSessionState {
    Available(PreparedSourceEnvelope),
    Persisting(Arc<PersistenceCompletion>),
    Started(Arc<PersistenceCompletion>),
}

enum BeginPreparedSession {
    Start {
        source: PreparedSourceEnvelope,
        completion: Arc<PersistenceCompletion>,
    },
    Wait(Arc<PersistenceCompletion>),
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
                .saturating_add(option_string_capacity(&candidate.primary_text))
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
        StoreError::ImportRunConflict => "run_conflict",
        StoreError::ImportRunNotResumable => "run_not_resumable",
        StoreError::ImportCheckpointMismatch => "checkpoint_failure",
        StoreError::ImportWorkerSuperseded => "worker_superseded",
        StoreError::ImportInvariant => "invalid_run_state",
        StoreError::InvalidImportInput | StoreError::ImportBatchTooLarge => "invalid_import",
        _ => "store_failure",
    };
    ImportError::service(reason)
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
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            PREPARED_SESSION_CAPACITY,
            4 * 1024,
            PREPARED_SESSION_CAPACITY * 4 * 1024,
        ));
        let now = Instant::now();
        let mut first = None;
        let mut newest = None;
        for index in 0..=PREPARED_SESSION_CAPACITY {
            let fingerprint_byte = u8::try_from(index).unwrap();
            let analysis_id = Uuid::now_v7();
            let source = empty_source(fingerprint_byte);
            let envelope = admitted_source(&mut sessions, &budget, source);
            sessions.insert(
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
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(1, 4 * 1024, 4 * 1024));
        let now = Instant::now();
        let analysis_id = Uuid::now_v7();
        let source = admitted_source(&mut sessions, &budget, empty_source(1));
        sessions.insert(analysis_id, source, now);

        assert!(matches!(
            sessions.begin(analysis_id, now + PREPARED_SESSION_TTL),
            BeginPreparedSession::Missing
        ));
        let ledger = budget.ledger.lock().unwrap();
        assert_eq!(ledger.source_count, 0);
        assert_eq!(ledger.retained_bytes, 0);
    }

    #[test]
    fn prepared_session_cache_evicts_by_retained_byte_budget() {
        let source = empty_source(1);
        let retained_bytes = source.retained_bytes();
        let budget = AdmissionBudget::new(ImportAdmissionLimits::new(
            8,
            retained_bytes,
            retained_bytes.saturating_mul(2).saturating_sub(1),
        ));
        let mut sessions = PreparedSessions::default();
        let now = Instant::now();
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let first_source = admitted_source(&mut sessions, &budget, source);
        sessions.insert(first, first_source, now);

        let second_source = admitted_source(&mut sessions, &budget, empty_source(2));
        sessions.insert(second, second_source, now + Duration::from_millis(1));

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
            retained_bytes.saturating_mul(2),
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
                primary_text: None,
                search_ocr: None,
                missing_payload: false,
                source_application_path: None,
            }],
            failure_counts: Vec::new(),
        }
    }
}
