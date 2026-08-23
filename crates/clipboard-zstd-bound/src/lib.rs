#![deny(unsafe_op_in_unsafe_fn)]

use std::fmt;

pub const REQUIRED_ZSTD_VERSION_NUMBER: u32 = 10_507;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EstimateError {
    VersionMismatch,
    InvalidEstimate,
}

impl fmt::Display for EstimateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::VersionMismatch => formatter.write_str("linked zstd version is not approved"),
            Self::InvalidEstimate => formatter.write_str("zstd context estimate failed"),
        }
    }
}

impl std::error::Error for EstimateError {}

unsafe extern "C" {
    fn ZSTD_estimateCCtxSize(max_compression_level: std::ffi::c_int) -> usize;
}

/// Returns zstd's documented one-shot CCtx upper budget before a context exists.
///
/// The exact `zstd-sys` dependency pins the native implementation to zstd 1.5.7. The version
/// check prevents a differently linked implementation from silently invalidating the proof.
pub fn compression_context_size(max_compression_level: i32) -> Result<usize, EstimateError> {
    // SAFETY: `ZSTD_versionNumber` has no arguments and no memory-safety preconditions.
    let version = unsafe { zstd_sys::ZSTD_versionNumber() };
    if version != REQUIRED_ZSTD_VERSION_NUMBER {
        return Err(EstimateError::VersionMismatch);
    }
    // SAFETY: `ZSTD_estimateCCtxSize` accepts every C `int` compression-level value and does not
    // dereference caller memory. Error results are checked with zstd's official predicate.
    let estimate = unsafe { ZSTD_estimateCCtxSize(max_compression_level) };
    // SAFETY: `ZSTD_isError` accepts every returned zstd size/error code.
    if unsafe { zstd_sys::ZSTD_isError(estimate) } != 0 {
        return Err(EstimateError::InvalidEstimate);
    }
    Ok(estimate)
}

#[cfg(test)]
mod tests {
    #[test]
    fn pinned_level_three_context_estimate_is_exact() {
        assert_eq!(super::compression_context_size(3).unwrap(), 1_303_568);
    }
}
