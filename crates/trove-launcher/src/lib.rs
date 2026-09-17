#![forbid(unsafe_code)]
//! The application catalog behind the palette's launcher mode.
//!
//! This crate is deliberately boring: it walks a list of directories the
//! caller owns, reads `Contents/Info.plist` out of every `*.app` it finds,
//! and hands back a sorted, bounded, deduplicated catalog. It never spawns a
//! process and never touches the network — deciding what to list and actually
//! launching it are separate powers, and only the shell (`trove-app`)
//! holds the second one.
//!
//! Every scan root is injected. The real roots come from [`default_scan_roots`]
//! at the shell's edge; tests pass temporary directories, which is why none of
//! them know anything about the machine they run on.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use trove_core::normalize_search_text;

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

/// Upper bound on an `Info.plist` worth reading. Real ones sit in kilobytes;
/// a "plist" past this is a payload wearing metadata's name, and the crate
/// treats bundle content as untrusted everywhere else — skipping the bundle
/// beats materializing the file into the clipboard process.
pub const MAX_INFO_PLIST_BYTES: u64 = 4 * 1024 * 1024;

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

/// One scan root, carrying the policy that decides what counts as a
/// launchable application inside it.
///
/// The policy exists because `LSUIElement` — "menu-bar agent, no Dock icon"
/// — means two different things depending on where the bundle lives. Under
/// `/System` it is the operating system's own machinery (Dock,
/// ControlCenter, AirPlayUIAgent: a hundred agents nobody launches by
/// name), and listing them would flood the catalog with rows Enter cannot
/// meaningfully start. In the folders a user owns it is how ordinary
/// applications say "I live in the menu bar": Raycast, Docker, a VPN in the
/// menu bar — Spotlight lists these, and a launcher that hides them reports
/// them as unfindable. `LSBackgroundOnly` stays unlisted everywhere: a
/// daemon with no face at all is not something anyone launches by name,
/// whichever folder it sits in.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanRoot {
    /// The directory the walk starts from.
    pub path: PathBuf,
    /// Whether `LSUIElement` bundles under this root are listed.
    pub list_menu_bar_agents: bool,
}

impl ScanRoot {
    /// A root whose bundles belong to the person using the machine:
    /// installed on purpose, agents included.
    pub fn user(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            list_menu_bar_agents: true,
        }
    }

    /// A root whose bundles belong to the operating system: agents there
    /// are machinery, not applications.
    pub fn system(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            list_menu_bar_agents: false,
        }
    }
}

/// An unannotated path is treated as a system root — the strict policy. A
/// caller that means otherwise says so with [`ScanRoot::user`], and injected
/// test roots keep the behavior they have always had.
impl From<PathBuf> for ScanRoot {
    fn from(path: PathBuf) -> Self {
        Self::system(path)
    }
}

/// The roots the palette launcher scans, in catalog order, with the
/// menu-bar-agent policy each carries. Missing ones (Setapp never installed,
/// no `~/Applications`) are simply absent from the scan; their absence is
/// not an error any user needs to be told about.
pub fn default_scan_roots() -> Vec<ScanRoot> {
    let mut roots = vec![
        ScanRoot::user("/Applications"),
        ScanRoot::system("/System/Applications"),
        // Mostly agents — the strict policy on this root is what keeps it
        // from flooding the list — but Finder lives here, and a launcher
        // that cannot find Finder has a hole in it.
        ScanRoot::system("/System/Library/CoreServices"),
        // The cryptex: modern macOS ships some applications (Safari is the
        // notable one) as a firmlink in `/Applications` whose canonical path
        // resolves here, outside every other root. Without this root the
        // catalog refuses them — and a launcher that cannot list Safari has
        // a hole the same size. Harmless where the path does not exist: the
        // scan canonicalizes each root and drops the ones that do not.
        ScanRoot::system("/System/Volumes/Preboot/Cryptexes/App/System/Applications"),
    ];
    if let Some(base) = directories::BaseDirs::new() {
        roots.push(ScanRoot::user(base.home_dir().join("Applications")));
    }
    roots.push(ScanRoot::user("/Applications/Setapp"));
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
pub fn scan_applications(roots: &[ScanRoot]) -> Vec<AppBundle> {
    // Canonical roots once, not per bundle: symlinks and firmlinks resolve
    // the same way for every candidate under a root.
    let mut by_path: BTreeMap<PathBuf, AppBundle> = BTreeMap::new();

    for root in roots {
        // An unreadable root is a fact about the filesystem, not a reason
        // to give up on the rest of the catalog.
        if let Ok(canonical_root) = fs::canonicalize(&root.path) {
            scan_directory(
                &canonical_root,
                &canonical_root,
                0,
                root.list_menu_bar_agents,
                &mut by_path,
            );
        }
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
    list_menu_bar_agents: bool,
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
            if let Some(bundle) = read_bundle(root, &path, &name, list_menu_bar_agents) {
                by_path.insert(PathBuf::from(&bundle.path), bundle);
            }
            // Either way, a bundle is sealed: never descend into it.
            continue;
        }
        if is_directory {
            scan_directory(root, &path, depth + 1, list_menu_bar_agents, by_path);
        }
    }
}

/// Turns one `*.app` directory into a catalog entry, or `None` when it should
/// not be listed at all.
fn read_bundle(
    root: &Path,
    path: &Path,
    name: &str,
    list_menu_bar_agents: bool,
) -> Option<AppBundle> {
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

    let plist_path = path.join("Contents").join("Info.plist");
    // Same discipline as every other bound in this crate: the plist is
    // bundle content, and bundle content is data, not something to be
    // trusted with the process's memory. A plist past the bound makes the
    // whole bundle unlistable — without reading it there is no honest way
    // to know it is not an agent, and listing by stem alone would put a
    // name in front of Enter that nothing vouches for.
    let plist_oversized = fs::metadata(&plist_path)
        .map(|metadata| metadata.len() > MAX_INFO_PLIST_BYTES)
        .unwrap_or(false);
    if plist_oversized {
        return None;
    }
    let info = read_info_plist(&plist_path);
    // Background-only daemons have no face anywhere: no Dock presence, no
    // menu bar, nothing launching them by hand could show. They stay
    // unlisted in user roots too — an entry Enter cannot show the result of
    // is a lie about every root it sits in. (Checked before `LSUIElement`
    // so a bundle flying both flags stays hidden even where agents list.)
    if plist_agent_flag(&info, "LSBackgroundOnly") {
        return None;
    }
    // Menu-bar agents: listed where the user put them, hidden where the
    // operating system did — see [`ScanRoot`]. The flag is read in both the
    // boolean form and the string form plists do ship, because a filter that
    // only understands one spelling is a filter some agents walk through.
    if !list_menu_bar_agents && plist_agent_flag(&info, "LSUIElement") {
        return None;
    }

    // The name the bridge validates is trimmed and non-empty, so the name
    // this side emits must be too — one malformed bundle must cost itself a
    // listing, never the whole catalog. A blank candidate falls through to
    // the next one, and the file stem is the name of last resort: the
    // bundle directory says what it is even when its metadata does not.
    let stem = name.strip_suffix(".app").unwrap_or(name).trim().to_owned();
    let display_name = non_blank_plist_string(&info, "CFBundleDisplayName")
        .or_else(|| non_blank_plist_string(&info, "CFBundleName"))
        .unwrap_or(stem);
    if display_name.is_empty() {
        // Not even the directory name says anything: there is no honest way
        // to list this bundle, so it is not listed.
        return None;
    }
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
fn read_info_plist(plist_path: &Path) -> Option<plist::Value> {
    let file = fs::File::open(plist_path).ok()?;
    plist::Value::from_reader(file).ok()
}

fn plist_string(info: &Option<plist::Value>, key: &str) -> Option<String> {
    info.as_ref()
        .and_then(plist::Value::as_dictionary)
        .and_then(|dictionary| dictionary.get(key))
        .and_then(plist::Value::as_string)
        .map(str::to_owned)
}

/// A plist string that is present but blank is not a name: trimming happens
/// per candidate so an empty `CFBundleDisplayName` cannot shadow a real
/// `CFBundleName` underneath it.
fn non_blank_plist_string(info: &Option<plist::Value>, key: &str) -> Option<String> {
    plist_string(info, key)
        .map(|candidate| candidate.trim().to_owned())
        .filter(|candidate| !candidate.is_empty())
}

/// Reads one `LS*` flag the way Launch Services effectively does: booleans
/// first, and the string spellings (`true`, `yes`, any case) plists really
/// do ship. A flag that is absent or false — in either form — is not set.
fn plist_agent_flag(info: &Option<plist::Value>, key: &str) -> bool {
    info.as_ref()
        .and_then(plist::Value::as_dictionary)
        .and_then(|dictionary| dictionary.get(key))
        .is_some_and(|value| {
            value.as_boolean() == Some(true)
                || value.as_string().is_some_and(|text| {
                    matches!(text.trim().to_ascii_lowercase().as_str(), "true" | "yes")
                })
        })
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
    use super::{
        MAX_APP_NAME_BYTES, MAX_APP_PATH_BYTES, MAX_CATALOG_APPS, truncate_on_char_boundary,
    };

    #[test]
    fn truncation_lands_on_a_character_boundary() {
        // `ał` is four bytes; a two-byte cap would otherwise split the `ł`.
        assert_eq!(truncate_on_char_boundary("ał", 2), "a");
        assert_eq!(truncate_on_char_boundary("short", 8), "short");
    }

    #[test]
    fn the_bounds_are_the_contract_the_bridge_validates() {
        // These numbers are mirrored in apps/desktop-ui/src/lib/contracts.ts
        // and asserted there in the same breath. A change on either side
        // must be a decision that updates both tests, not a silent drift
        // that only surfaces as a rejected catalog.
        assert_eq!(MAX_APP_NAME_BYTES, 256);
        assert_eq!(MAX_APP_PATH_BYTES, 1_024);
        assert_eq!(MAX_CATALOG_APPS, 2_000);
    }
}
