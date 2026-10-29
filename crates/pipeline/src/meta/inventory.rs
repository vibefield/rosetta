//! Read bounded metadata into one unpublished plan. Full admission and
//! dependency-scoped policy checks share these exact adapter/driver semantics.

use super::*;

/// Where one inventory file goes, decided from its stream alone.
enum Entry<'a> {
    /// No stream claims the path (an inventory walked without classes).
    Unclaimed,
    InventoryOnly,
    /// A driver the stream's declared layout cannot carry.
    Misconfigured(&'a StreamSpec, StreamDriver),
    /// A SQLite source, snapshotted and admitted by the ordered merge.
    Database(&'a StreamSpec, StreamDriver),
    /// A transcript or document whose bytes are read on the skim pool.
    Read(&'a StreamSpec, StreamDriver),
}

fn classify<'a>(
    adapter: &dyn AgentAdapter,
    layout: &'a rosetta_adapter_support::GroundTruthDoc,
    file: &rosetta_capture::ScannedFile,
) -> Entry<'a> {
    let Some(stream) = crate::discover::inventory_stream(layout, file) else {
        return Entry::Unclaimed;
    };
    let Some(driver) = adapter.stream_driver(&stream.id) else {
        return Entry::InventoryOnly;
    };
    match driver {
        StreamDriver::JsonLines
            if stream.file_format == FileFormat::Jsonl
                && stream.discipline == FileDiscipline::AppendOnly =>
        {
            Entry::Read(stream, driver)
        }
        StreamDriver::DocumentSnapshot
            if matches!(
                stream.file_format,
                FileFormat::Json | FileFormat::Toml | FileFormat::Markdown | FileFormat::Text
            ) && stream.discipline == FileDiscipline::RewriteInPlace =>
        {
            Entry::Read(stream, driver)
        }
        StreamDriver::SqliteSnapshot
            if stream.file_format == FileFormat::Sqlite
                && stream.discipline == FileDiscipline::Database =>
        {
            Entry::Database(stream, driver)
        }
        _ => Entry::Misconfigured(stream, driver),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_inventory(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    inventory: &[rosetta_capture::ScannedFile],
    opts: &IngestOptions,
    instrument: &Instrument,
    threads: usize,
    report: &mut MetaReport,
    output: &mut MetaOutput,
) -> Result<(), PipelineError> {
    let layout = adapter.layout();
    let entries: Vec<Entry<'_>> = inventory
        .iter()
        .map(|file| classify(adapter, layout, file))
        .collect();
    report.files_matched += entries
        .iter()
        .filter(|entry| !matches!(entry, Entry::Unclaimed))
        .count();
    let reads: Vec<(&rosetta_capture::ScannedFile, &StreamSpec, StreamDriver)> = inventory
        .iter()
        .zip(&entries)
        .filter_map(|(file, entry)| match *entry {
            Entry::Read(stream, driver) => Some((file, stream, driver)),
            _ => None,
        })
        .collect();
    crate::skim::in_order(
        &reads,
        threads,
        |&(file, stream, driver)| match driver {
            StreamDriver::DocumentSnapshot => {
                crate::skim::read_document(adapter, root, file, stream, opts, instrument)
            }
            _ => crate::skim::skim_transcript(adapter, root, file, stream, opts, instrument),
        },
        |next| -> Result<(), PipelineError> {
            for (file, entry) in inventory.iter().zip(&entries) {
                let (stream, driver) = match *entry {
                    Entry::Unclaimed => continue,
                    Entry::InventoryOnly => {
                        report.files_inventory_only += 1;
                        continue;
                    }
                    Entry::Misconfigured(stream, driver) => {
                        return Err(PipelineError::Config(format!(
                            "{} opted stream {} into {:?}, but its layout is {:?}/{:?}",
                            adapter.kind(),
                            stream.id,
                            driver,
                            stream.file_format,
                            stream.discipline
                        )));
                    }
                    Entry::Database(stream, driver) => {
                        // Sidecars (-wal/-shm/-journal) travel with their
                        // database; only the database itself is a source.
                        if rosetta_capture::is_db_sidecar(&file.relative_path) {
                            report.files_inventory_only += 1;
                            continue;
                        }
                        match instrument.time(Metric::SkimNanos, || {
                            rosetta_capture::sqlite_file_status(&root.join(&file.relative_path))
                        })? {
                            rosetta_capture::SqliteFileStatus::Missing => {
                                report.files_vanished += 1;
                                instrument.add(Metric::FilesVanished, 1);
                                continue;
                            }
                            rosetta_capture::SqliteFileStatus::NotDatabase => {
                                // Broad database globs also see agent-owned
                                // lock and marker files. Preserve them in the
                                // inventory count, but never manufacture an
                                // empty SQLite source.
                                report.files_inventory_only += 1;
                                continue;
                            }
                            rosetta_capture::SqliteFileStatus::Database => {}
                        }
                        admit_database(
                            adapter,
                            runtime,
                            root,
                            stream,
                            driver,
                            &file.relative_path,
                            file.size,
                            file.modified_ms,
                            report,
                            output,
                            instrument,
                        )?;
                        continue;
                    }
                    Entry::Read(stream, driver) => (stream, driver),
                };
                match next()? {
                    crate::skim::Skimmed::Vanished => {
                        report.files_vanished += 1;
                        instrument.add(Metric::FilesVanished, 1);
                    }
                    crate::skim::Skimmed::Unfinished => {
                        report.files_skimmed += 1;
                        report.files_unfinished += 1;
                        instrument.add(Metric::FilesUnfinished, 1);
                    }
                    crate::skim::Skimmed::Transcript(skim) => {
                        report.files_skimmed += 1;
                        admit_skim(
                            runtime, report, output, file, stream, driver, *skim, false, instrument,
                        );
                    }
                    // One physical document may enumerate several native
                    // sessions, each an independently replayable virtual
                    // source (`<document-path>#<native-id>`). A member the
                    // document no longer lists keeps its last projected
                    // generation, like a removed SQLite member and a deleted
                    // JSONL transcript (product decision, 2026-09-24).
                    crate::skim::Skimmed::Document(skims) => {
                        report.files_skimmed += 1;
                        for skim in skims {
                            admit_skim(
                                runtime, report, output, file, stream, driver, skim, true,
                                instrument,
                            );
                        }
                    }
                }
            }
            Ok(())
        },
    )?;

    Ok(())
}
