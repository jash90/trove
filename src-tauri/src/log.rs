//! One line at a time, somewhere a person can find it afterwards.
//!
//! A bundled application's standard error goes nowhere anyone looks: launched
//! from Finder or at login, `eprintln!` reaches no terminal. Every degraded
//! start — a shortcut someone else holds, a menu bar item that would not
//! appear, a data folder that would not open — used to be reported only there.
//! This appends the same lines to `~/Library/Logs/Trove/trove.log`, where
//! Console.app lists it, and still writes them to standard error for
//! `pnpm tauri dev`.

use std::io::Write;
use std::path::PathBuf;

/// Where the log lives, or nothing when there is no home directory to put it in.
pub fn log_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library")
            .join("Logs")
            .join("Trove")
            .join("trove.log"),
    )
}

/// Records one line, prefixed with the time it happened.
///
/// Failing to write the log is not itself worth reporting anywhere: the line
/// still went to standard error, and there is no third place to complain to.
pub fn log_line(message: &str) {
    eprintln!("trove: {message}");
    let Some(path) = log_path() else {
        return;
    };
    if let Some(directory) = path.parent() {
        let _ = std::fs::create_dir_all(directory);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    let _ = writeln!(file, "{} {message}", unix_seconds());
}

/// Seconds since the epoch, which is all a log read beside a crash needs to
/// line the two up, and needs no date library to produce.
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}
