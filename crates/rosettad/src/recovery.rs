//! Recoverable retirement of an interrupted first projection.
//!
//! A projection with `canonical_ready = 0` was public only as a progressive
//! cold-start surface. It is neither the raw source of truth nor safe to open
//! as an ordinary canonical database. The production app chooses to restart
//! that work through the fast RAM path. Before it does, this module moves the
//! entire incomplete artifact set into a named recovery directory.
//!
//! The move is a tiny filesystem transaction. A durable marker names every
//! artifact before the first rename. If the process or machine stops between
//! renames, the next invocation resumes the same plan while holding the
//! projection writer lock. The marker is removed only after both directory
//! sides and a permanent recovery manifest are durable.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

const RECOVERY_FORMAT: u64 = 1;
const RECOVERY_ROOT: &str = "recovery";
const PERMANENT_MANIFEST: &str = "recovery.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IncompleteRecovery {
    pub(crate) path: PathBuf,
    /// Consent for DISCOVERED kinds, read off the archived projection's
    /// `~`-rooted enabled rows.
    pub(crate) enabled_agents: BTreeSet<String>,
    /// Supervisor-enrolled runtimes the archived projection held (absolute
    /// roots). Recorded for the record only: their consent is this start's
    /// `--enroll` file, never the archive.
    pub(crate) enrolled_runtimes: Vec<RecoveredRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveredRuntime {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) data_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecoveryPlan {
    db_name: String,
    reason: String,
    recovery_id: String,
    artifacts: Vec<String>,
    enabled_agents: BTreeSet<String>,
    enrolled_runtimes: Vec<RecoveredRuntime>,
}

/// Retire an interrupted projection and its sibling artifacts without losing
/// evidence. The caller must hold the projection writer lock for the full
/// call. A complete projection is never moved.
pub(crate) fn restart_incomplete_projection(
    db: &Path,
    ledger: &Path,
) -> Result<Option<IncompleteRecovery>, String> {
    let parent = artifact_parent(db)?;
    let marker = appended(db, ".restart.json");

    let plan = if marker.exists() {
        read_plan(&marker, db, ledger)?
    } else {
        if db.exists() {
            let inspected = inspect_projection(db)?;
            if inspected.canonical_ready {
                return Ok(None);
            }
            create_plan(
                db,
                ledger,
                "canonical_ready=false",
                inspected.enabled_agents,
                inspected.enrolled_runtimes,
                &marker,
            )?
        } else {
            let existing = existing_artifacts(db, ledger)?;
            if existing.is_empty() {
                return Ok(None);
            }
            create_plan(
                db,
                ledger,
                "projection-missing",
                BTreeSet::new(),
                Vec::new(),
                &marker,
            )?
        }
    };

    let recovery = parent.join(RECOVERY_ROOT).join(&plan.recovery_id);
    std::fs::create_dir_all(&recovery).map_err(|error| {
        format!(
            "creating recovery directory {}: {error}",
            recovery.display()
        )
    })?;

    for name in &plan.artifacts {
        let source = parent.join(name);
        let destination = recovery.join(name);
        match (path_exists(&source)?, path_exists(&destination)?) {
            (true, false) => std::fs::rename(&source, &destination).map_err(|error| {
                format!(
                    "moving incomplete artifact {} to {}: {error}; restart with --restart-incomplete to resume",
                    source.display(),
                    destination.display()
                )
            })?,
            (false, true) => {
                // A prior invocation already completed this seam.
            }
            (true, true) => {
                return Err(format!(
                    "incomplete recovery has both source {} and destination {}; refusing to choose",
                    source.display(),
                    destination.display()
                ));
            }
            (false, false) => {
                return Err(format!(
                    "incomplete recovery lost planned artifact {} from both source and destination",
                    name
                ));
            }
        }
    }

    let bytes = plan_bytes(&plan)?;
    let permanent = recovery.join(PERMANENT_MANIFEST);
    match std::fs::read(&permanent) {
        Ok(existing) if existing == bytes => {}
        Ok(_) => {
            return Err(format!(
                "recovery manifest {} disagrees with the durable restart marker",
                permanent.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_new_synced(&permanent, &bytes)?;
        }
        Err(error) => {
            return Err(format!(
                "reading recovery manifest {}: {error}",
                permanent.display()
            ));
        }
    }

    // The backup is not acknowledged until every renamed directory entry and
    // the permanent manifest are durable. The stable marker is therefore a
    // sufficient recovery instruction after a crash at any earlier seam.
    sync_directory(&recovery)?;
    sync_directory(&parent.join(RECOVERY_ROOT))?;
    sync_directory(parent)?;
    std::fs::remove_file(&marker).map_err(|error| {
        format!(
            "removing completed recovery marker {}: {error}",
            marker.display()
        )
    })?;
    sync_directory(parent)?;

    Ok(Some(IncompleteRecovery {
        path: recovery,
        enabled_agents: plan.enabled_agents,
        enrolled_runtimes: plan.enrolled_runtimes,
    }))
}

fn create_plan(
    db: &Path,
    ledger: &Path,
    reason: &str,
    enabled_agents: BTreeSet<String>,
    enrolled_runtimes: Vec<RecoveredRuntime>,
    marker: &Path,
) -> Result<RecoveryPlan, String> {
    let parent = artifact_parent(db)?;
    let db_name = utf8_file_name(db, "projection")?;
    let artifacts = existing_artifacts(db, ledger)?;
    if artifacts.is_empty() {
        return Err(format!(
            "incomplete artifact set for {} disappeared before recovery",
            db.display()
        ));
    }

    let recovery_root = parent.join(RECOVERY_ROOT);
    std::fs::create_dir_all(&recovery_root).map_err(|error| {
        format!(
            "creating recovery root {}: {error}",
            recovery_root.display()
        )
    })?;
    let recovery_id = unique_recovery_id(&recovery_root)?;
    let recovery = recovery_root.join(&recovery_id);
    std::fs::create_dir(&recovery).map_err(|error| {
        format!(
            "creating recovery directory {}: {error}",
            recovery.display()
        )
    })?;
    sync_directory(&recovery_root)?;
    sync_directory(parent)?;

    let plan = RecoveryPlan {
        db_name,
        reason: reason.to_owned(),
        recovery_id,
        artifacts,
        enabled_agents,
        enrolled_runtimes,
    };
    write_new_synced(marker, &plan_bytes(&plan)?)?;
    sync_directory(parent)?;
    Ok(plan)
}

fn read_plan(marker: &Path, db: &Path, ledger: &Path) -> Result<RecoveryPlan, String> {
    let bytes = std::fs::read(marker)
        .map_err(|error| format!("reading recovery marker {}: {error}", marker.display()))?;
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parsing recovery marker {}: {error}", marker.display()))?;
    let object = value
        .as_object()
        .ok_or_else(|| format!("recovery marker {} is not an object", marker.display()))?;
    if object.get("format").and_then(Value::as_u64) != Some(RECOVERY_FORMAT) {
        return Err(format!(
            "recovery marker {} has an unsupported format",
            marker.display()
        ));
    }
    let db_name = required_string(object.get("projection"), "projection", marker)?;
    if db_name != utf8_file_name(db, "projection")? {
        return Err(format!(
            "recovery marker {} belongs to another projection",
            marker.display()
        ));
    }
    let reason = required_string(object.get("reason"), "reason", marker)?;
    if reason != "canonical_ready=false" && reason != "projection-missing" {
        return Err(format!(
            "recovery marker {} has an unsupported reason",
            marker.display()
        ));
    }
    let recovery_id = required_string(object.get("recovery"), "recovery", marker)?;
    validate_component(&recovery_id, "recovery id", marker)?;

    let allowed = allowed_artifacts(db, ledger)?;
    let artifacts = required_string_array(object.get("artifacts"), "artifacts", marker)?;
    if artifacts.is_empty() || artifacts.iter().any(|name| !allowed.contains(name)) {
        return Err(format!(
            "recovery marker {} names an unexpected artifact",
            marker.display()
        ));
    }
    let unique: BTreeSet<_> = artifacts.iter().collect();
    if unique.len() != artifacts.len() {
        return Err(format!(
            "recovery marker {} has duplicate artifacts",
            marker.display()
        ));
    }

    let enabled_agents =
        required_string_array(object.get("enabled_agents"), "enabled_agents", marker)?
            .into_iter()
            .collect();
    // Markers written before 2026-09-12 carry no enrolled list; absent reads
    // as empty, since the file is those runtimes' consent anyway.
    let enrolled_runtimes = match object.get("enrolled_runtimes") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| {
                format!(
                    "recovery marker {} has a non-array enrolled_runtimes",
                    marker.display()
                )
            })?
            .iter()
            .map(|entry| {
                let object = entry.as_object().ok_or_else(|| {
                    format!(
                        "recovery marker {} has a non-object enrolled runtime",
                        marker.display()
                    )
                })?;
                Ok(RecoveredRuntime {
                    id: required_string(object.get("id"), "enrolled_runtimes.id", marker)?,
                    kind: required_string(object.get("kind"), "enrolled_runtimes.kind", marker)?,
                    data_root: required_string(
                        object.get("data_root"),
                        "enrolled_runtimes.data_root",
                        marker,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, String>>()?,
    };
    Ok(RecoveryPlan {
        db_name,
        reason,
        recovery_id,
        artifacts,
        enabled_agents,
        enrolled_runtimes,
    })
}

struct InspectedProjection {
    canonical_ready: bool,
    enabled_agents: BTreeSet<String>,
    enrolled_runtimes: Vec<RecoveredRuntime>,
}

fn inspect_projection(db: &Path) -> Result<InspectedProjection, String> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(db, flags).map_err(|error| {
        format!(
            "opening existing projection {} read-only: {error}",
            db.display()
        )
    })?;
    let ready: i64 = connection
        .query_row(
            "SELECT canonical_ready FROM projection_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|error| {
            format!(
                "reading canonical readiness from {}: {error}; refusing automatic recovery",
                db.display()
            )
        })?;
    if ready != 0 && ready != 1 {
        return Err(format!(
            "projection {} has invalid canonical_ready {ready}",
            db.display()
        ));
    }
    // An enabled row with the layout's `~` root is the user's consent for
    // that kind's discovered runtime. An enabled row with an absolute root is
    // a supervisor-enrolled runtime: recorded, but never consent for the
    // native runtime of its kind.
    let mut statement = connection
        .prepare("SELECT id, kind, data_root FROM agent_runtime WHERE enabled = 1 ORDER BY id")
        .map_err(|error| format!("reading enrollment from {}: {error}", db.display()))?;
    let mut enabled_agents = BTreeSet::new();
    let mut enrolled_runtimes = Vec::new();
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| format!("querying enrollment from {}: {error}", db.display()))?;
    for row in rows {
        let (id, kind, data_root) =
            row.map_err(|error| format!("decoding enrollment from {}: {error}", db.display()))?;
        if crate::runtime_ledger::is_layout_root(&data_root) {
            enabled_agents.insert(kind);
        } else {
            enrolled_runtimes.push(RecoveredRuntime {
                id,
                kind,
                data_root,
            });
        }
    }
    Ok(InspectedProjection {
        canonical_ready: ready == 1,
        enabled_agents,
        enrolled_runtimes,
    })
}

fn allowed_artifacts(db: &Path, ledger: &Path) -> Result<BTreeSet<String>, String> {
    let parent = artifact_parent(db)?;
    if artifact_parent(ledger)? != parent {
        return Err(format!(
            "--restart-incomplete requires the raw ledger beside the projection; {} is outside {}",
            ledger.display(),
            parent.display()
        ));
    }
    let db_name = utf8_file_name(db, "projection")?;
    let ledger_name = utf8_file_name(ledger, "ledger")?;
    Ok([
        db_name.clone(),
        format!("{db_name}-wal"),
        format!("{db_name}-shm"),
        format!("{db_name}-journal"),
        format!("{db_name}.assets"),
        format!("{db_name}.search"),
        ledger_name,
    ]
    .into_iter()
    .collect())
}

fn existing_artifacts(db: &Path, ledger: &Path) -> Result<Vec<String>, String> {
    let parent = artifact_parent(db)?;
    let mut artifacts = Vec::new();
    for name in allowed_artifacts(db, ledger)? {
        let path = parent.join(&name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(format!(
                        "refusing incomplete recovery through symlink {}",
                        path.display()
                    ));
                }
                artifacts.push(name);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "stating recovery artifact {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(artifacts)
}

fn plan_bytes(plan: &RecoveryPlan) -> Result<Vec<u8>, String> {
    let enrolled_runtimes: Vec<Value> = plan
        .enrolled_runtimes
        .iter()
        .map(|runtime| {
            json!({
                "id": runtime.id,
                "kind": runtime.kind,
                "data_root": runtime.data_root,
            })
        })
        .collect();
    let mut bytes = serde_json::to_vec_pretty(&json!({
        "format": RECOVERY_FORMAT,
        "reason": plan.reason,
        "projection": plan.db_name,
        "recovery": plan.recovery_id,
        "artifacts": plan.artifacts,
        "enabled_agents": plan.enabled_agents,
        "enrolled_runtimes": enrolled_runtimes,
    }))
    .map_err(|error| format!("serializing incomplete recovery plan: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn required_string(value: Option<&Value>, field: &str, marker: &Path) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("recovery marker {} has no string {field}", marker.display()))
}

fn required_string_array(
    value: Option<&Value>,
    field: &str,
    marker: &Path,
) -> Result<Vec<String>, String> {
    value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("recovery marker {} has no array {field}", marker.display()))?
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                format!(
                    "recovery marker {} has a non-string {field} entry",
                    marker.display()
                )
            })
        })
        .collect()
}

fn validate_component(value: &str, label: &str, marker: &Path) -> Result<(), String> {
    let mut components = Path::new(value).components();
    let valid = matches!(components.next(), Some(Component::Normal(component)) if component == std::ffi::OsStr::new(value))
        && components.next().is_none();
    if valid {
        Ok(())
    } else {
        Err(format!(
            "recovery marker {} has an unsafe {label}",
            marker.display()
        ))
    }
}

fn unique_recovery_id(root: &Path) -> Result<String, String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    for attempt in 0..100_u32 {
        let id = format!("incomplete-{millis}-{}-{attempt}", std::process::id());
        if !root.join(&id).exists() {
            return Ok(id);
        }
    }
    Err(format!(
        "could not allocate a recovery directory in {}",
        root.display()
    ))
}

fn artifact_parent(path: &Path) -> Result<&Path, String> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            format!(
                "artifact {} needs an explicit parent directory",
                path.display()
            )
        })
}

fn utf8_file_name(path: &Path, label: &str) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .ok_or_else(|| format!("{label} path {} needs a UTF-8 file name", path.display()))
}

fn appended(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn path_exists(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("stating {}: {error}", path.display())),
    }
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if path_exists(path)? {
        return Err(format!("refusing to replace existing {}", path.display()));
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(format!(".tmp-{}", std::process::id()));
    let temp = PathBuf::from(temp);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(|error| format!("creating {}: {error}", temp.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("persisting {}: {error}", temp.display()))?;
    drop(file);
    std::fs::rename(&temp, path).map_err(|error| {
        format!(
            "publishing complete marker {} from {}: {error}",
            path.display(),
            temp.display()
        )
    })
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("syncing directory {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosetta_store::Store;

    /// A consented DISCOVERED runtime: the layout's `~` root.
    fn enable(store: &Store, kind: &str) {
        store
            .conn()
            .execute(
                "INSERT INTO agent_runtime
                   (id, kind, data_root, enabled, discovered_at)
                 VALUES (?1, ?2, ?3, 1, 0)",
                rusqlite::params![format!("runtime-{kind}"), kind, format!("~/.{kind}")],
            )
            .unwrap();
    }

    /// A supervisor-enrolled runtime: an absolute root.
    fn enroll(store: &Store, kind: &str, root: &str) {
        store
            .conn()
            .execute(
                "INSERT INTO agent_runtime
                   (id, kind, data_root, enabled, discovered_at)
                 VALUES (?1, ?2, ?3, 1, 0)",
                rusqlite::params![format!("enrolled-{kind}"), kind, root],
            )
            .unwrap();
    }

    #[test]
    fn complete_projection_is_never_moved() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("rosetta.db");
        drop(Store::open(&db).unwrap());
        let ledger = db.with_extension("ledger");

        assert_eq!(restart_incomplete_projection(&db, &ledger).unwrap(), None);
        assert!(db.is_file());
        assert!(!temp.path().join(RECOVERY_ROOT).exists());
    }

    #[test]
    fn incomplete_projection_artifacts_and_enrollment_are_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("rosetta.db");
        let store = Store::open_progressive(&db).unwrap();
        enable(&store, "claude-code");
        enroll(&store, "claude-code", "/users/u1/agents/kim");
        drop(store);
        let ledger = db.with_extension("ledger");
        std::fs::create_dir(&ledger).unwrap();
        std::fs::write(ledger.join("evidence"), b"raw").unwrap();
        let assets = appended(&db, ".assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(assets.join("member"), b"asset").unwrap();
        let search = appended(&db, ".search");
        std::fs::create_dir(&search).unwrap();

        let recovered = restart_incomplete_projection(&db, &ledger)
            .unwrap()
            .expect("incomplete projection is retired");
        // The enrolled row is recorded but is NOT consent for the native
        // claude-code runtime: only the `~` row counts as a kind.
        assert_eq!(
            recovered.enabled_agents,
            BTreeSet::from(["claude-code".to_string()])
        );
        assert_eq!(
            recovered.enrolled_runtimes,
            vec![RecoveredRuntime {
                id: "enrolled-claude-code".into(),
                kind: "claude-code".into(),
                data_root: "/users/u1/agents/kim".into(),
            }]
        );
        assert!(!db.exists());
        assert!(!ledger.exists());
        assert!(!assets.exists());
        assert!(!search.exists());
        assert_eq!(
            std::fs::read(recovered.path.join("rosetta.ledger/evidence")).unwrap(),
            b"raw"
        );
        assert_eq!(
            std::fs::read(recovered.path.join("rosetta.db.assets/member")).unwrap(),
            b"asset"
        );
        assert!(recovered.path.join("rosetta.db.search").is_dir());
        assert!(recovered.path.join(PERMANENT_MANIFEST).is_file());
        assert!(!appended(&db, ".restart.json").exists());
    }

    #[test]
    fn a_partial_recovery_resumes_from_its_durable_marker() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("rosetta.db");
        let store = Store::open_progressive(&db).unwrap();
        enable(&store, "codex");
        drop(store);
        let ledger = db.with_extension("ledger");
        std::fs::create_dir(&ledger).unwrap();
        let marker = appended(&db, ".restart.json");
        let inspected = inspect_projection(&db).unwrap();
        let plan = create_plan(
            &db,
            &ledger,
            "canonical_ready=false",
            inspected.enabled_agents,
            inspected.enrolled_runtimes,
            &marker,
        )
        .unwrap();
        let recovery = temp.path().join(RECOVERY_ROOT).join(&plan.recovery_id);
        std::fs::rename(&db, recovery.join("rosetta.db")).unwrap();

        let recovered = restart_incomplete_projection(&db, &ledger)
            .unwrap()
            .expect("marker resumes despite the missing source db");
        assert_eq!(recovered.path, recovery);
        assert!(recovery.join("rosetta.db").is_file());
        assert!(recovery.join("rosetta.ledger").is_dir());
        assert_eq!(
            recovered.enabled_agents,
            BTreeSet::from(["codex".to_string()])
        );
        assert!(!marker.exists());
    }

    #[test]
    fn a_ledger_published_before_its_projection_is_also_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("rosetta.db");
        let ledger = db.with_extension("ledger");
        std::fs::create_dir(&ledger).unwrap();
        std::fs::write(ledger.join("durable-frame"), b"frame").unwrap();

        let recovered = restart_incomplete_projection(&db, &ledger)
            .unwrap()
            .expect("orphan ledger is an interrupted publication");
        assert!(recovered.enabled_agents.is_empty());
        assert!(!ledger.exists());
        assert_eq!(
            std::fs::read(recovered.path.join("rosetta.ledger/durable-frame")).unwrap(),
            b"frame"
        );
    }
}
