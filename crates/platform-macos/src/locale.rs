//! The languages the user prefers, most preferred first.
//!
//! Read from the system rather than from the bundle: the application ships its
//! own strings, so what matters is what the person asked for in System
//! Settings, not which `.lproj` folders happen to be in the bundle.

/// BCP 47 tags such as `pl-PL` or `en-GB`, most preferred first. Empty when
/// the list cannot be read.
#[cfg(target_os = "macos")]
pub fn preferred_languages() -> Vec<String> {
    objc2_foundation::NSLocale::preferredLanguages()
        .iter()
        .map(|language| language.to_string())
        .collect()
}

/// Off macOS there is no list to read here; the caller falls back to the
/// POSIX locale variables.
#[cfg(not(target_os = "macos"))]
pub fn preferred_languages() -> Vec<String> {
    Vec::new()
}
