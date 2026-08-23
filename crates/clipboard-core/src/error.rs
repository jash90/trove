use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid UTF-8 text payload")]
    InvalidUtf8,
    #[error("canonicalization_too_complex")]
    CanonicalizationTooComplex,
}
