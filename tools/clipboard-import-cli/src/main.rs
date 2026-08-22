#![forbid(unsafe_code)]

mod path_policy;
mod verify;

use std::{collections::BTreeMap, io, io::Write, path::PathBuf};

use clap::{Parser, Subcommand, error::ErrorKind};
use clipboard_core::ContentKind;
use clipboard_import::{
    ImportError, ImportParseReport, ImportService, ImportSource, detect_export, parse_export_report,
};
use clipboard_store::{StoreError, StoreHandle};
use serde::Serialize;

use path_policy::prepare_import_paths;
use verify::{VerifyOutput, verify};

#[derive(Parser)]
#[command(name = "clipboard-import-cli")]
#[command(about = "Private clipboard export importer")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Analyze {
        #[arg(long)]
        source: PathBuf,
    },
    Import {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        data_dir: PathBuf,
    },
    Verify {
        #[arg(long)]
        data_dir: PathBuf,
        #[arg(long)]
        expect_records: u64,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
enum CommandOutput {
    Analyze(AnalyzeOutput),
    Import(ImportOutput),
    Verify(VerifyOutput),
}

struct CommandExecution {
    output: CommandOutput,
    success: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AnalyzeOutput {
    status: &'static str,
    source_kind: &'static str,
    total: u64,
    candidate_records: u64,
    failed: u64,
    counts_by_kind: Vec<KindCount>,
    available_image_records: u64,
    missing_image_records: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportOutput {
    status: &'static str,
    total: u64,
    imported: u64,
    already_present: u64,
    skipped: u64,
    failed: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct KindCount {
    pub(crate) kind: &'static str,
    pub(crate) event_count: u64,
    pub(crate) missing_payload_count: u64,
}

#[derive(Serialize)]
struct ErrorOutput {
    status: &'static str,
    code: &'static str,
}

#[derive(Clone, Copy)]
pub(crate) struct CliFailure {
    code: &'static str,
}

impl CliFailure {
    pub(crate) const fn new(code: &'static str) -> Self {
        Self { code }
    }
}

#[tokio::main]
async fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return;
        }
        Err(_) => {
            emit_error(CliFailure::new("invalid_arguments"));
            std::process::exit(2);
        }
    };

    match execute(cli.command).await {
        Ok(execution) => {
            if emit_json(io::stdout().lock(), &execution.output).is_err() {
                emit_error(CliFailure::new("output_unavailable"));
                std::process::exit(1);
            }
            if !execution.success {
                std::process::exit(1);
            }
        }
        Err(error) => {
            emit_error(error);
            std::process::exit(1);
        }
    }
}

async fn execute(command: Commands) -> Result<CommandExecution, CliFailure> {
    match command {
        Commands::Analyze { source } => Ok(CommandExecution {
            output: CommandOutput::Analyze(analyze(&source)?),
            success: true,
        }),
        Commands::Import { source, data_dir } => Ok(CommandExecution {
            output: CommandOutput::Import(import(&source, &data_dir).await?),
            success: true,
        }),
        Commands::Verify {
            data_dir,
            expect_records,
        } => {
            let output = verify(&data_dir, expect_records)?;
            let success = output.is_success();
            Ok(CommandExecution {
                output: CommandOutput::Verify(output),
                success,
            })
        }
    }
}

fn analyze(source: &std::path::Path) -> Result<AnalyzeOutput, CliFailure> {
    let source = path_policy::canonical_source(source)?;
    let detected = detect_export(&source).map_err(import_failure)?;
    let report = parse_export_report(&source).map_err(import_failure)?;
    let (counts_by_kind, available_image_records, missing_image_records) =
        analyzed_counts(&report)?;
    Ok(AnalyzeOutput {
        status: "ok",
        source_kind: source_name(detected.source),
        total: count_from_usize(report.total)?,
        candidate_records: count_from_usize(report.candidates.len())?,
        failed: count_from_usize(report.failures.len())?,
        counts_by_kind,
        available_image_records,
        missing_image_records,
    })
}

async fn import(
    source: &std::path::Path,
    data_dir: &std::path::Path,
) -> Result<ImportOutput, CliFailure> {
    let paths = prepare_import_paths(source, data_dir)?;
    let store = StoreHandle::open(paths.store_config()).map_err(store_failure)?;
    path_policy::verify_created_storage(&paths)?;
    let service = ImportService::new(store);
    let summary = service
        .run_to_completion(&paths.source)
        .await
        .map_err(import_failure)?;
    Ok(ImportOutput {
        status: "ok",
        total: summary.total,
        imported: summary.imported,
        already_present: summary.already_present,
        skipped: summary.skipped,
        failed: summary.failed,
    })
}

fn analyzed_counts(report: &ImportParseReport) -> Result<(Vec<KindCount>, u64, u64), CliFailure> {
    let mut kind_counts = BTreeMap::<&'static str, (u64, u64)>::new();
    let mut available_images = 0_u64;
    let mut missing_images = 0_u64;
    for candidate in &report.candidates {
        let kind = candidate.capture.kind.as_str();
        let (event_count, missing_payload_count) = kind_counts.entry(kind).or_default();
        *event_count = event_count
            .checked_add(1)
            .ok_or_else(|| CliFailure::new("count_overflow"))?;
        if candidate.missing_payload {
            *missing_payload_count = missing_payload_count
                .checked_add(1)
                .ok_or_else(|| CliFailure::new("count_overflow"))?;
        }
        if candidate.capture.kind == ContentKind::Image {
            if candidate.missing_payload {
                missing_images = missing_images
                    .checked_add(1)
                    .ok_or_else(|| CliFailure::new("count_overflow"))?;
            } else {
                available_images = available_images
                    .checked_add(1)
                    .ok_or_else(|| CliFailure::new("count_overflow"))?;
            }
        }
    }
    Ok((
        kind_counts
            .into_iter()
            .map(|(kind, (event_count, missing_payload_count))| KindCount {
                kind,
                event_count,
                missing_payload_count,
            })
            .collect(),
        available_images,
        missing_images,
    ))
}

const fn source_name(source: ImportSource) -> &'static str {
    match source {
        ImportSource::Raycast => "raycast",
        ImportSource::SuperCmd => "supercmd",
    }
}

fn count_from_usize(value: usize) -> Result<u64, CliFailure> {
    u64::try_from(value).map_err(|_| CliFailure::new("count_overflow"))
}

fn import_failure(error: ImportError) -> CliFailure {
    let code = match error {
        ImportError::Export { reason, .. }
        | ImportError::Record { reason, .. }
        | ImportError::Service { reason } => reason,
    };
    CliFailure::new(code)
}

pub(crate) fn store_failure(error: StoreError) -> CliFailure {
    match error {
        StoreError::DatabaseMissing => CliFailure::new("database_missing"),
        StoreError::IncompatibleSchema | StoreError::UnsupportedSchemaVersion(_) => {
            CliFailure::new("incompatible_database")
        }
        StoreError::StorageBoundary => CliFailure::new("unsafe_storage_layout"),
        _ => CliFailure::new("storage_unavailable"),
    }
}

fn emit_error(error: CliFailure) {
    let _ = emit_json(
        io::stderr().lock(),
        &ErrorOutput {
            status: "error",
            code: error.code,
        },
    );
}

fn emit_json(mut output: impl Write, value: &impl Serialize) -> io::Result<()> {
    serde_json::to_writer(&mut output, value).map_err(io::Error::other)?;
    output.write_all(b"\n")
}
