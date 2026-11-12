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
                    crate::skim::Skimmed::Transcript(skim, cut) => {
                        report.files_skimmed += 1;
                        admit_skim(
                            adapter,
                            runtime,
                            report,
                            output,
                            file,
                            stream,
                            driver,
                            *skim,
                            false,
                            Some(cut),
                            instrument,
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
                                adapter, runtime, report, output, file, stream, driver, skim, true,
                                None, instrument,
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

/// Meta admission for a `SqliteSnapshot` stream: enumerate the database's
/// sessions via the adapter's declared plan and admit each as a VIRTUAL
/// source (`<db-path>#<native-id>`) — its own cursor, its own generation
/// swaps, its own ledger entries, exactly like a file.
#[allow(clippy::too_many_arguments)]
fn admit_database(
    adapter: &dyn AgentAdapter,
    runtime: &RuntimeRow,
    root: &Path,
    stream: &rosetta_adapter_support::StreamSpec,
    driver: StreamDriver,
    db_relative: &str,
    size: u64,
    modified_ms: i64,
    report: &mut MetaReport,
    output: &mut MetaOutput,
    instrument: &Instrument,
) -> Result<(), PipelineError> {
    let Some(plan) = adapter.snapshot_plan(&stream.id) else {
        return Err(PipelineError::Config(format!(
            "{} drives stream {} as SqliteSnapshot but declares no snapshot plan",
            adapter.kind(),
            stream.id
        )));
    };
    let outcome = instrument.time(Metric::SkimNanos, || {
        rosetta_capture::snapshot_db(&root.join(db_relative))
    })?;
    let snapshot = match outcome {
        rosetta_capture::SnapshotOutcome::Ready(snapshot) => snapshot,
        // A racing writer or vanished db is the stream phase's problem.
        rosetta_capture::SnapshotOutcome::Missing | rosetta_capture::SnapshotOutcome::Unstable => {
            report.files_vanished += 1;
            instrument.add(Metric::FilesVanished, 1);
            return Ok(());
        }
    };
    report.files_skimmed += 1;
    instrument.add(Metric::FilesSkimmed, 1);

    for row in snapshot.query_rows(&plan.sessions_sql, None)? {
        let value: serde_json::Value = serde_json::from_str(&row).map_err(|error| {
            PipelineError::Config(format!("sessions_sql row is not an object: {error}"))
        })?;
        let skim = adapter.snapshot_session(&stream.id, &value);
        let Some(native) = skim.session_id.clone() else {
            if output.admission.excludes_cwd(skim.cwd.as_deref()) {
                continue;
            }
            report.files_unidentified += 1;
            instrument.add(Metric::FilesUnidentified, 1);
            continue;
        };
        if let Some(selection) = adapter.transcript_selection(&stream.id, &value) {
            output.selections.insert(native.clone(), selection);
        }
        let updated = skim.updated_at.unwrap_or(modified_ms);
        admit_identified(
            runtime,
            output,
            AdmitSource {
                relative_path: format!("{db_relative}#{native}"),
                stream_id: stream.id.clone(),
                file_format: stream.file_format,
                discipline: stream.discipline,
                driver,
                size,
                modified_ms: updated,
                cut: None,
            },
            skim,
            native,
            instrument,
        );
    }

    // A member no longer returned by sessions_sql keeps its last projected
    // generation, exactly as a deleted JSONL transcript keeps its rows: an
    // agent pruning its own store (Hermes 0.21.4 retains 90 days by default)
    // must not erase Rosetta's history (product decision, 2026-09-24). The
    // cursor simply sees no further activity.
    Ok(())
}
