#![forbid(unsafe_code)]
//! The application catalog behind the palette's launcher mode.
//!
//! This crate is deliberately boring: it walks a list of directories the
//! caller owns, reads `Contents/Info.plist` out of every `*.app` it finds,
//! and hands back a sorted, bounded, deduplicated catalog. It never spawns a
//! process and never touches the network — deciding what to list and actually
//! launching it are separate powers, and only the shell (`clipboard-history-app`)
//! holds the second one.
//!
//! Every scan root is injected. The real roots come from [`default_scan_roots`]
//! at the shell's edge; tests pass temporary directories, which is why none of
//! them know anything about the machine they run on.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use clipboard_core::normalize_search_text;
use serde::Serialize;

/// A hard ceiling on the catalog. A real machine sits near six hundred
/// bundles; the cap exists so a pathological root (a mounted build farm, a
/// loop of symlinks resolved by the filesystem itself) degrades into a long
/// list rather than an unbounded one.
pub const MAX_CATALOG_APPS: usize = 2_000;

/// Bound on the display name carried across the bridge. Names past this are
/// truncated on a character boundary, never rejected: a long name is a fact
/// about the bundle, not a reason to pretend it is not installed.
pub const MAX_APP_NAME_BYTES: usize = 256;

/// Bound on any bundle path the crate accepts or emits. Paths past this are
/// skipped at scan time and refused at launch time.
pub const MAX_APP_PATH_BYTES: usize = 1_024;

/// How deep the scanner descends below each root. One level below the root
/// exists for `/Applications/Utilities` and its system twin; two would only
/// wade into vendor folder trees nobody launches from.
pub const SCAN_DEPTH: usize = 2;

/// One launchable application, in the shape the palette receives it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppBundle {
    /// `CFBundleDisplayName`, else `CFBundleName`, else the file stem.
    pub name: String,
    /// `CFBundleIdentifier`, when the bundle declares one.
    pub bundle_id: Option<String>,
    /// The canonical filesystem path of the `.app` directory.
    pub path: String,
}

/// Why [`validate_launch_path`] refused a candidate. The codes are stable
/// machine identifiers for the bridge; the paths that triggered them never
/// travel back, for the same reason no other local path does.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchPathError {
    Invalid,
    NotFound,
    NotAppBundle,
    OutsideScannedRoots,
}

impl LaunchPathError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "launch_invalid",
            Self::NotFound => "app_not_found",
            Self::NotAppBundle => "app_not_launchable",
            Self::OutsideScannedRoots => "app_outside_roots",
        }
    }
}

/// The roots the palette launcher scans, in catalog order. Missing ones
/// (Setapp never installed, no `~/Applications`) are simply absent from the
/// scan; their absence is not an error any user needs to be told about.
pub fn default_scan_roots() -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
        // Mostly agents — the `LSUIElement` filter below is what keeps this
        // root from flooding the list — but Finder lives here, and a launcher
        // that cannot find Finder has a hole in it.
        PathBuf::from("/System/Library/CoreServices"),
    ];
    if let Some(base) = directories::BaseDirs::new() {
        roots.push(base.home_dir().join("Applications"));
    }
    roots.push(PathBuf::from("/Applications/Setapp"));
    roots
}

/// Scans `roots` for application bundles and returns the sorted, deduplicated
/// catalog, capped at [`MAX_CATALOG_APPS`].
///
/// The scan never descends into a `*.app` directory (a bundle is a sealed
/// unit; its passengers are not separately launchable), skips bundles the
/// filesystem cannot canonicalize (broken symlinks), skips non-UTF-8 paths
/// (they cannot round-trip through the bridge's strings), and drops bundles
/// whose canonical path escapes the root they were found under — the launch
/// validator would refuse exactly those, and listing an application Enter
/// cannot start is worse than not listing it.
pub fn scan_applications(roots: &[PathBuf]) -> Vec<AppBundle> {
    // Canonical roots once, not per bundle: symlinks and firmlinks resolve
    // the same way for every candidate under a root.
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .collect();
    // Deduped by canonical path: whichever directory entry found a bundle
    // first wins, so an alias next to the original lists it once.
    let mut by_path: BTreeMap<PathBuf, AppBundle> = BTreeMap::new();

    for root in &roots {
        scan_directory(root, root, 0, &mut by_path);
    }

    let mut catalog: Vec<AppBundle> = by_path.into_values().collect();
    catalog.sort_by(|a, b| {
        let a_key = (normalize_search_text(&a.name), a.name.as_str());
        let b_key = (normalize_search_text(&b.name), b.name.as_str());
        a_key.cmp(&b_key)
    });
    catalog.truncate(MAX_CATALOG_APPS);
    catalog
}

/// Reads one directory of a root. `depth` counts levels below the root, so
/// the root's own entries are at depth 1 — [`SCAN_DEPTH`] of 2 therefore
/// covers `/Applications/Utilities/Terminal.app` and stops there.
fn scan_directory(
    root: &Path,
    directory: &Path,
    depth: usize,
    by_path: &mut BTreeMap<PathBuf, AppBundle>,
) {
    if depth >= SCAN_DEPTH {
        return;
    }
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        // An unreadable directory is a fact about the filesystem, not a
        // reason to give up on the rest of the catalog.
        Err(_) => return,
    };
    for entry in entries.flatten() {
        // Non-UTF-8 names cannot cross the bridge; skipping the bundle is
        // honest, mangling the name is not.
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let path = directory.join(&name);
        // `file_type` does not follow symlinks, and bundle aliases are
        // symlinks by nature — dirness for a bundle is decided on its
        // canonical target inside `read_bundle`, not here. Plain folders are
        // only recursed when they really are folders, so a directory symlink
        // cannot loop the walk.
        let is_directory = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        if name.ends_with(".app") {
            if let Some(bundle) = read_bundle(root, &path, &name) {
                by_path.insert(PathBuf::from(&bundle.path), bundle);
            }
            // Either way, a bundle is sealed: never descend into it.
            continue;
        }
        if is_directory {
            scan_directory(root, &path, depth + 1, by_path);
        }
    }
}

/// Turns one `*.app` directory into a catalog entry, or `None` when it should
/// not be listed at all.
fn read_bundle(root: &Path, path: &Path, name: &str) -> Option<AppBundle> {
    let canonical = fs::canonicalize(path).ok()?;
    // A file wearing the `.app` suffix, or a symlink pointing at one: the
    // catalog lists bundles, and the filesystem is the arbiter of what is.
    if !canonical.is_dir() {
        return None;
    }
    let path_string = canonical.to_str()?;
    if path_string.len() > MAX_APP_PATH_BYTES {
        return None;
    }
    // The launch validator only accepts bundles under a canonical root, so
    // the catalog refuses them here too — listing what Enter cannot start is
    // the worse half of a lie.
    if !canonical.starts_with(root) {
        return None;
    }

    let info = read_info_plist(path);
    // Faceless helpers: `LSUIElement` agents and `LSBackgroundOnly` daemons
    // have no Dock presence and no icon to show. Without this check one
    // system root alone adds hundreds of them.
    if plist_bool(&info, "LSUIElement") == Some(true)
        || plist_bool(&info, "LSBackgroundOnly") == Some(true)
    {
        return None;
    }

    let display_name = plist_string(&info, "CFBundleDisplayName")
        .or_else(|| plist_string(&info, "CFBundleName"))
        // The file stem is a name of last resort — the bundle directory says
        // what it is even when its metadata does not.
        .unwrap_or_else(|| name.strip_suffix(".app").unwrap_or(name).to_owned());
    let bundle_id = plist_string(&info, "CFBundleIdentifier")
        .map(|id| truncate_on_char_boundary(&id, MAX_APP_NAME_BYTES))
        .filter(|id| !id.is_empty());

    Some(AppBundle {
        name: truncate_on_char_boundary(&display_name, MAX_APP_NAME_BYTES),
        bundle_id,
        path: path_string.to_owned(),
    })
}

/// A missing or unreadable `Info.plist` is a bundle state, not a scan error:
/// the stem still names it and the catalog still lists it.
fn read_info_plist(path: &Path) -> Option<plist::Value> {
    let file = fs::File::open(path.join("Contents").join("Info.plist")).ok()?;
    plist::Value::from_reader(file).ok()
}

fn plist_string(info: &Option<plist::Value>, key: &str) -> Option<String> {
    info.as_ref()
        .and_then(plist::Value::as_dictionary)
        .and_then(|dictionary| dictionary.get(key))
        .and_then(plist::Value::as_string)
        .map(str::to_owned)
}

fn plist_bool(info: &Option<plist::Value>, key: &str) -> Option<bool> {
    info.as_ref()
        .and_then(plist::Value::as_dictionary)
        .and_then(|dictionary| dictionary.get(key))
        .and_then(plist::Value::as_boolean)
}

fn truncate_on_char_boundary(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

/// Checks that `candidate` names a launchable bundle under one of `roots`
/// and returns its canonical path.
///
/// This is the gate in front of every launch: the string arrives from the
/// webview, and the webview can be made to say anything. A path that is
/// empty, relative, NUL-bearing, oversized, absent, not a bundle directory,
/// or outside the canonicalized scan roots is refused with a stable code —
/// and without echoing the path back.
pub fn validate_launch_path(
    candidate: &str,
    roots: &[PathBuf],
) -> Result<PathBuf, LaunchPathError> {
    if candidate.is_empty() || candidate.len() > MAX_APP_PATH_BYTES || candidate.contains('\0') {
        return Err(LaunchPathError::Invalid);
    }
    let candidate_path = Path::new(candidate);
    if !candidate_path.is_absolute() {
        return Err(LaunchPathError::Invalid);
    }
    let canonical = fs::canonicalize(candidate_path).map_err(|_| LaunchPathError::NotFound)?;
    if !canonical.is_dir() {
        return Err(LaunchPathError::NotAppBundle);
    }
    if canonical
        .extension()
        .and_then(|extension| extension.to_str())
        != Some("app")
    {
        return Err(LaunchPathError::NotAppBundle);
    }
    let under_a_root = roots.iter().any(|root| {
        fs::canonicalize(root)
            .map(|root| canonical.starts_with(root))
            .unwrap_or(false)
    });
    if !under_a_root {
        return Err(LaunchPathError::OutsideScannedRoots);
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::truncate_on_char_boundary;

    #[test]
    fn truncation_lands_on_a_character_boundary() {
        // `ał` is four bytes; a two-byte cap would otherwise split the `ł`.
        assert_eq!(truncate_on_char_boundary("ał", 2), "a");
        assert_eq!(truncate_on_char_boundary("short", 8), "short");
    }
}
