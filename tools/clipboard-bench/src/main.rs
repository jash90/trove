//! Measures a clipboard history at a size no one will type by hand.
//!
//! History is unbounded by default, so "does it still work at a million
//! records" is a gate rather than a curiosity. This tool builds a synthetic
//! history of any size and then measures the two things a user feels: how long
//! a search takes, and whether scrolling the whole list stays flat in memory.
//!
//! Every payload is generated from a seed. No real clipboard data is read,
//! written, or reported.

mod dataset;

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use clap::{Parser, Subcommand};
use clipboard_search::{HistoryCursor, SearchRequest, SearchStoreExt};
use clipboard_store::{
    BLOB_DIRECTORY_NAME, DATABASE_FILENAME, GcStepBudget, StoreConfig, StoreHandle,
};
use dataset::{Dataset, Seed};
use serde::Serialize;

/// How many records are written between progress lines.
const PROGRESS_EVERY: u64 = 25_000;

/// How many rows one list page holds, matching the interface.
const PAGE_SIZE: u32 = 50;

/// How many pages one pagination sweep walks before it stops.
///
/// Walking a million rows fifty at a time is twenty thousand queries and is not
/// what the interface does; what matters is that the last page costs what the
/// first one did.
const PAGINATION_PAGES: u32 = 400;

/// How many blob directory entries one reclamation pass looks at, matching the
/// bound the application's maintenance timer uses.
const GC_ENTRIES_PER_PASS: usize = 2_000;

#[derive(Parser)]
#[command(
    name = "clipboard-bench",
    about = "Synthetic clipboard history benchmarks"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Writes a synthetic history into a data directory.
    Generate {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        records: u64,
        #[arg(long, default_value_t = 42)]
        seed: u64,
    },
    /// Measures search, pagination, and maintenance against an existing one.
    Measure {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long, default_value_t = 500)]
        queries: u32,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

fn main() -> Result<(), String> {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("runtime unavailable: {error}"))?;
    match cli.command {
        Commands::Generate {
            data_dir,
            records,
            seed,
        } => runtime.block_on(generate(&data_dir, records, Seed(seed))),
        Commands::Measure {
            data_dir,
            queries,
            output,
        } => measure(&data_dir, queries, output.as_deref()),
    }
}

async fn generate(data_dir: &Path, records: u64, seed: Seed) -> Result<(), String> {
    std::fs::create_dir_all(data_dir).map_err(|error| format!("data directory: {error}"))?;
    let store = open(data_dir)?;
    let dataset = Dataset::new(seed, records);
    let started = Instant::now();
    for index in 0..records {
        store
            .ingest(dataset.record(index))
            .await
            .map_err(|error| format!("ingest failed at record {index}: {error}"))?;
        if (index + 1).is_multiple_of(PROGRESS_EVERY) {
            let elapsed = started.elapsed();
            eprintln!(
                "{} / {records} records, {:.0} records/s",
                index + 1,
                (index + 1) as f64 / elapsed.as_secs_f64().max(f64::EPSILON)
            );
        }
    }
    let elapsed = started.elapsed();
    println!(
        "{}",
        serde_json::to_string_pretty(&GenerateReport {
            records,
            seed: seed.0,
            digest: format!("{:016x}", Dataset::new(seed, records.min(10_000)).digest()),
            elapsed_ms: elapsed.as_millis() as u64,
            records_per_second: records as f64 / elapsed.as_secs_f64().max(f64::EPSILON),
        })
        .map_err(|error| error.to_string())?
    );
    Ok(())
}

fn measure(data_dir: &Path, queries: u32, output: Option<&Path>) -> Result<(), String> {
    let store = open(data_dir)?;
    let stats = store
        .stats()
        .map_err(|error| format!("statistics unavailable: {error}"))?;

    // One untimed pass, so what follows measures a warm cache rather than a
    // cold page cache the user would only ever see once.
    let _ = store.search(SearchRequest::from_text("Łódź"));

    let report = MeasureReport {
        events: stats.event_count,
        contents: stats.content_count,
        search: timed_searches(&store, queries)?,
        first_page_ms: duration_ms(time_first_page(&store)?),
        pagination: paginate(&store)?,
        blob_reclamation_ms: duration_ms(time_reclamation_scan(&store)?),
        storage: storage_sizes(data_dir),
        resident_bytes: resident_bytes(),
    };
    let rendered = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
    if let Some(path) = output {
        std::fs::write(path, &rendered).map_err(|error| format!("report: {error}"))?;
    }
    println!("{rendered}");
    Ok(())
}

/// The queries a benchmark rotates through.
///
/// A single repeated query would measure one cached plan. These differ in
/// selectivity, in diacritics, and in whether they match at all.
const QUERY_TERMS: [&str; 8] = [
    "Łódź",
    "lodz",
    "gęślą",
    "notatka",
    "synthetic",
    "wrzesień",
    "example",
    "nieistniejące",
];

fn timed_searches(store: &StoreHandle, queries: u32) -> Result<SearchReport, String> {
    let mut timings = Vec::with_capacity(queries as usize);
    let mut matched = 0_u64;
    for index in 0..queries {
        let term = QUERY_TERMS[(index as usize) % QUERY_TERMS.len()];
        let started = Instant::now();
        let page = store
            .search(SearchRequest::from_text(term))
            .map_err(|error| format!("search failed: {error}"))?;
        timings.push(started.elapsed());
        matched += page.items.len() as u64;
    }
    timings.sort_unstable();
    Ok(SearchReport {
        queries,
        matched_rows: matched,
        p50_ms: duration_ms(percentile(&timings, 50)),
        p95_ms: duration_ms(percentile(&timings, 95)),
        p99_ms: duration_ms(percentile(&timings, 99)),
        max_ms: duration_ms(timings.last().copied().unwrap_or_default()),
    })
}

/// The cost of the very first page, which is what a summoned window waits on.
fn time_first_page(store: &StoreHandle) -> Result<Duration, String> {
    let started = Instant::now();
    store
        .search(SearchRequest {
            query: String::new(),
            limit: PAGE_SIZE,
            cursor: None,
            include_do_not_index: false,
        })
        .map_err(|error| format!("first page failed: {error}"))?;
    Ok(started.elapsed())
}

/// Walks the list by cursor and reports what the first and last pages cost.
///
/// This is the measurement that catches a growing `OFFSET`: with keyset
/// pagination the last page costs what the first one did, and without it the
/// walk degrades the deeper it goes.
fn paginate(store: &StoreHandle) -> Result<PaginationReport, String> {
    let mut cursor: Option<HistoryCursor> = None;
    let mut timings = Vec::with_capacity(PAGINATION_PAGES as usize);
    let mut rows = 0_u64;
    for _ in 0..PAGINATION_PAGES {
        let started = Instant::now();
        let page = store
            .search(SearchRequest {
                query: String::new(),
                limit: PAGE_SIZE,
                cursor,
                include_do_not_index: false,
            })
            .map_err(|error| format!("pagination failed: {error}"))?;
        timings.push(started.elapsed());
        rows += page.items.len() as u64;
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    let first = timings.first().copied().unwrap_or_default();
    let last = timings.last().copied().unwrap_or_default();
    timings.sort_unstable();
    Ok(PaginationReport {
        pages: timings.len() as u32,
        rows,
        first_page_ms: duration_ms(first),
        last_page_ms: duration_ms(last),
        p95_page_ms: duration_ms(percentile(&timings, 95)),
    })
}

/// One bounded reclamation pass, the same shape the maintenance timer runs.
fn time_reclamation_scan(store: &StoreHandle) -> Result<Duration, String> {
    let Ok(cas) = store.cas_store() else {
        return Ok(Duration::ZERO);
    };
    let Ok(mut session) = cas.start_gc() else {
        return Ok(Duration::ZERO);
    };
    let started = Instant::now();
    // One reader for the whole pass, matching what the application's
    // maintenance timer does — per-file readers would measure connection
    // setup rather than the scan.
    let mut outcome = None;
    store
        .with_reader(|connection| {
            outcome = Some(
                session.step(GcStepBudget::new(GC_ENTRIES_PER_PASS), |relpath| {
                    Ok(is_referenced(connection, relpath))
                }),
            );
            Ok(())
        })
        .map_err(|error| format!("reclamation reader unavailable: {error}"))?;
    outcome
        .transpose()
        .map_err(|error| format!("reclamation scan failed: {error}"))?;
    Ok(started.elapsed())
}

fn is_referenced(connection: &rusqlite::Connection, relpath: &str) -> bool {
    connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM raw_payload WHERE blob_relpath = ?1)
                 OR EXISTS(SELECT 1 FROM artifact WHERE blob_relpath = ?1)",
            [relpath],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(true)
}

fn open(data_dir: &Path) -> Result<StoreHandle, String> {
    StoreHandle::open(StoreConfig::in_data_dir(data_dir))
        .map_err(|error| format!("store unavailable: {error}"))
}

fn storage_sizes(data_dir: &Path) -> StorageReport {
    let database = data_dir.join(DATABASE_FILENAME);
    StorageReport {
        database_bytes: file_bytes(&database),
        wal_bytes: file_bytes(&data_dir.join(format!("{DATABASE_FILENAME}-wal"))),
        blob_bytes: directory_bytes(&data_dir.join(BLOB_DIRECTORY_NAME)),
    }
}

fn file_bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0)
}

fn directory_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_bytes(&entry.path()),
            Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

/// Resident memory, where the platform exposes it without a dependency.
fn resident_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        return Some(kilobytes * 1024);
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        let kilobytes: u64 = String::from_utf8(output.stdout).ok()?.trim().parse().ok()?;
        return Some(kilobytes * 1024);
    }
    #[allow(unreachable_code)]
    None
}

fn percentile(sorted: &[Duration], percentile: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = (sorted.len() * percentile).div_ceil(100).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GenerateReport {
    records: u64,
    seed: u64,
    digest: String,
    elapsed_ms: u64,
    records_per_second: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MeasureReport {
    events: i64,
    contents: i64,
    search: SearchReport,
    first_page_ms: f64,
    pagination: PaginationReport,
    blob_reclamation_ms: f64,
    storage: StorageReport,
    resident_bytes: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SearchReport {
    queries: u32,
    matched_rows: u64,
    p50_ms: f64,
    p95_ms: f64,
    p99_ms: f64,
    max_ms: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PaginationReport {
    pages: u32,
    rows: u64,
    first_page_ms: f64,
    last_page_ms: f64,
    p95_page_ms: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageReport {
    database_bytes: u64,
    wal_bytes: u64,
    blob_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_percentile_never_reads_past_the_end() {
        let samples = [
            Duration::from_millis(1),
            Duration::from_millis(2),
            Duration::from_millis(3),
            Duration::from_millis(4),
        ];

        assert_eq!(percentile(&samples, 50), Duration::from_millis(2));
        assert_eq!(percentile(&samples, 95), Duration::from_millis(4));
        assert_eq!(percentile(&samples, 100), Duration::from_millis(4));
        assert_eq!(percentile(&[], 95), Duration::ZERO);
    }
}
