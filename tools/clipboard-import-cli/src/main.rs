#![forbid(unsafe_code)]

mod path_policy;
mod verify;

use std::{io, io::Write, path::PathBuf};

use clap::{Parser, Subcommand, error::ErrorKind};
use clipboard_import::{ImportError, ImportService, analyze_export};
use clipboard_store::{CasError, StorageBoundaryError, StoreError, StoreHandle};
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
    let analysis = analyze_export(&source).map_err(import_failure)?;
    Ok(AnalyzeOutput {
        status: "ok",
        source_kind: analysis.source.as_str(),
        total: analysis.total,
        candidate_records: analysis.candidate_records,
        failed: analysis.failed,
        counts_by_kind: analysis
            .counts_by_kind
            .into_iter()
            .map(|count| KindCount {
                kind: count.kind.as_str(),
                event_count: count.event_count,
                missing_payload_count: count.missing_payload_count,
            })
            .collect(),
        available_image_records: analysis.available_image_records,
        missing_image_records: analysis.missing_image_records,
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
        StoreError::PrivateStorageUnavailable
        | StoreError::Cas(CasError::PrivateStorageUnavailable) => {
            CliFailure::new("private_storage_unavailable")
        }
        _ => CliFailure::new("storage_unavailable"),
    }
}

pub(crate) fn boundary_failure(error: StorageBoundaryError) -> CliFailure {
    match error {
        StorageBoundaryError::Changed => CliFailure::new("unsafe_storage_layout"),
        StorageBoundaryError::PrivateStorageUnavailable => {
            CliFailure::new("private_storage_unavailable")
        }
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

#[cfg(test)]
mod tests {
    use clipboard_store::{CasError, StorageBoundaryError, StoreError};

    use super::{boundary_failure, store_failure};

    #[test]
    fn private_storage_errors_have_one_stable_cli_code() {
        for (code, actual) in [
            (
                "private_storage_unavailable",
                store_failure(StoreError::PrivateStorageUnavailable).code,
            ),
            (
                "private_storage_unavailable",
                store_failure(StoreError::Cas(CasError::PrivateStorageUnavailable)).code,
            ),
            (
                "private_storage_unavailable",
                boundary_failure(StorageBoundaryError::PrivateStorageUnavailable).code,
            ),
            (
                "unsafe_storage_layout",
                boundary_failure(StorageBoundaryError::Changed).code,
            ),
        ] {
            assert_eq!(actual, code);
        }
    }
}
