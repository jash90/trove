use std::sync::{Arc, Condvar, Mutex, OnceLock};

use thiserror::Error;

pub const MAX_IMPORT_OPERATION_BYTES: usize = 64 * 1024 * 1024;

const _: () = assert!(
    crate::writer::MAX_IMPORT_BATCH_BYTES + crate::writer::MAX_IMPORT_WRITER_SCRATCH_BYTES
        == MAX_IMPORT_OPERATION_BYTES
);

#[derive(Debug, Error)]
pub enum ImportOperationError {
    #[error("invalid import operation capacity")]
    InvalidCapacity,
    #[error("import operation gate is unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct ImportOperationGate {
    inner: Arc<ImportOperationGateInner>,
}

struct ImportOperationGateInner {
    capacity: usize,
    active_bytes: Mutex<usize>,
    released: Condvar,
}

impl ImportOperationGate {
    pub fn process_wide() -> Self {
        static GATE: OnceLock<ImportOperationGate> = OnceLock::new();
        GATE.get_or_init(|| {
            Self::with_capacity(MAX_IMPORT_OPERATION_BYTES)
                .expect("the fixed import operation capacity is valid")
        })
        .clone()
    }

    #[doc(hidden)]
    pub fn with_capacity(capacity: usize) -> Result<Self, ImportOperationError> {
        if capacity == 0 {
            return Err(ImportOperationError::InvalidCapacity);
        }
        Ok(Self {
            inner: Arc::new(ImportOperationGateInner {
                capacity,
                active_bytes: Mutex::new(0),
                released: Condvar::new(),
            }),
        })
    }

    pub fn acquire_blocking(&self) -> Result<ImportOperationPermit, ImportOperationError> {
        let mut active_bytes = self
            .inner
            .active_bytes
            .lock()
            .map_err(|_| ImportOperationError::Unavailable)?;
        while *active_bytes != 0 {
            active_bytes = self
                .inner
                .released
                .wait(active_bytes)
                .map_err(|_| ImportOperationError::Unavailable)?;
        }
        *active_bytes = self.inner.capacity;
        Ok(ImportOperationPermit {
            inner: Arc::clone(&self.inner),
        })
    }

    #[doc(hidden)]
    pub fn active_bytes(&self) -> usize {
        *self
            .inner
            .active_bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }
}

pub struct ImportOperationPermit {
    inner: Arc<ImportOperationGateInner>,
}

impl ImportOperationPermit {
    pub fn reserved_bytes(&self) -> usize {
        self.inner.capacity
    }
}

impl Drop for ImportOperationPermit {
    fn drop(&mut self) {
        let mut active_bytes = self
            .inner
            .active_bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *active_bytes = 0;
        self.inner.released.notify_one();
    }
}
