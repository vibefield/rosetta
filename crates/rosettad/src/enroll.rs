//! Supervisor-enrolled runtimes (2026-09-12): the `--enroll FILE` contract,
//! its validation, and the runtime TARGET list every serve mode iterates.
//!
//! A sibling product runs one rosettad per user and manages agent personas:
//! one vendor data root per persona under the user's own directory, bound to
//! the agent process through the vendor's root variable. A user therefore has
//! several roots of one kind — the OS-native `~/.claude` (discovered on the
//! roll call, consented by the user) plus N managed roots the supervisor owns.
//! The file names those roots. Its runtimes are consented by construction:
//! never on the roll call, never part of the one-shot consent gate, always
//! ingested and watched. `enabled_kinds` is the headless stand-in for the
//! roll call — consent for DISCOVERED kinds, given at start — and unlike
//! `--agent` it never narrows the adapter registry the roll call shows.
//!
//! The file is AUTHORITATIVE (2026-09-12): where it is given, consent for
//! discovered kinds is exactly `enabled_kinds` ∪ `--agent`/`--all-agents`, and
//! the projection's `enabled` flags are a projection OF it rather than a
//! second source. The file is the supervisor's record of what the user chose,
//! so it has to be able to REVOKE: a union with the projection could only ever
//! add, and a kind the user un-checked would keep being watched from a cache.
//! A pending cold generation gives way to it the same way — see
//! `serve::generation_consent`.
//!
//! Read once at start. Live mutation of the enrolled set and a rediscover
//! without restart are deliberately out of scope: the supervisor restarts the
//! daemon when the set changes (docs/ARCHITECTURE.md §11).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rosetta_adapter_support::AgentAdapter;
use rosetta_core::ids;
use rosetta_core::rows::RuntimeRow;
use rosetta_pipeline::{
    CensusRoot, Discovery, IngestOptions, Instrument, discover_roots, expand_data_root,
};
use rosetta_server::{DiscoveryRow, RuntimeIdentity, RuntimeProvenance};
use serde::Deserialize;

pub(crate) const ENROLLMENT_FORMAT: u32 = 1;

/// One `runtimes[]` entry as written by the supervisor. Unknown fields are
/// ignored at every level (serde's default), so the file may grow.
#[derive(Debug, Clone, Deserialize)]
struct EnrolledRootSpec {
    kind: String,
    data_root: String,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EnrollmentFile {
    format: u32,
    #[serde(default)]
    enabled_kinds: Vec<String>,
    #[serde(default)]
    runtimes: Vec<EnrolledRootSpec>,
}

/// One validated enrolled root: a registered adapter over an existing
/// absolute directory, with the identity the projection will carry.
#[derive(Clone)]
pub(crate) struct EnrolledRuntime {
    pub adapter: &'static dyn AgentAdapter,
    /// `data_root` is the absolute path exactly as the file spelled it; the
    /// id is `runtime_id(kind, data_root)`, so the spelling IS the identity.
    pub runtime: RuntimeRow,
    pub root: PathBuf,
    pub label: Option<String>,
}

impl std::fmt::Debug for EnrolledRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrolledRuntime")
            .field("kind", &self.runtime.kind)
            .field("data_root", &self.runtime.data_root)
            .field("id", &self.runtime.id)
            .field("label", &self.label)
            .finish()
    }
}

/// The file, read and validated once at start.
#[derive(Debug, Clone, Default)]
pub(crate) struct Enrollment {
    pub path: PathBuf,
    /// Consent for discovered kinds. Unknown or uninstalled kinds are warned
    /// about at resolution time, never fatal.
    pub enabled_kinds: Vec<String>,
    pub runtimes: Vec<EnrolledRuntime>,
}

impl Enrollment {
    /// Read, parse, and validate. Every refusal names the entry.
    pub fn load(
        path: &Path,
        registry: &[&'static dyn AgentAdapter],
        home: &Path,
    ) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("reading --enroll {}: {error}", path.display()))?;
        Self::parse(&text, path, registry, home)
    }

    pub fn parse(
        text: &str,
        path: &Path,
        registry: &[&'static dyn AgentAdapter],
        home: &Path,
    ) -> Result<Self, String> {
        let file: EnrollmentFile = serde_json::from_str(text).map_err(|error| {
            format!(
                "--enroll {}: not a valid enrollment file: {error}",
                path.display()
            )
        })?;
        if file.format != ENROLLMENT_FORMAT {
            return Err(format!(
                "--enroll {}: unsupported enrollment format {} (this daemon reads format {ENROLLMENT_FORMAT})",
                path.display(),
                file.format
            ));
        }
        let mut runtimes: Vec<EnrolledRuntime> = Vec::with_capacity(file.runtimes.len());
        for (index, spec) in file.runtimes.iter().enumerate() {
            let entry = |message: String| {
                format!(
                    "--enroll {}: runtime #{} (kind {:?}, data_root {:?}): {message}",
                    path.display(),
                    index + 1,
                    spec.kind,
                    spec.data_root
                )
            };
            let adapter = registry
                .iter()
                .copied()
                .find(|adapter| adapter.kind() == spec.kind)
                .ok_or_else(|| entry("unknown agent kind".to_string()))?;
            let root = Path::new(&spec.data_root);
            if spec.data_root.is_empty() || spec.data_root.starts_with('~') || !root.is_absolute() {
                return Err(entry(
                    "data_root must be an absolute path (`~` forms are refused)".to_string(),
                ));
            }
            match std::fs::metadata(root) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(entry("data_root is not a directory".to_string())),
                Err(error) => {
                    return Err(entry(format!(
                        "data_root is not an existing directory ({error}); the supervisor enrolls a root after creating it"
                    )));
                }
            }
            for (earlier_index, earlier) in runtimes.iter().enumerate() {
                if earlier.root == root {
                    return Err(entry(format!(
                        "duplicates runtime #{} ({:?})",
                        earlier_index + 1,
                        earlier.runtime.data_root
                    )));
                }
                if root.starts_with(&earlier.root) || earlier.root.starts_with(root) {
                    return Err(entry(format!(
                        "is nested with runtime #{} ({:?}); one root may not be walked by two runtimes",
                        earlier_index + 1,
                        earlier.runtime.data_root
                    )));
                }
            }
            // The kind's own layout root is the DISCOVERED runtime of that
            // kind: it enrolls through `enabled_kinds` (or the roll call),
            // never as a managed root — and nothing may nest with it either,
            // or the native walk and this one would double-ingest.
            let native = expand_data_root(&adapter.layout().data_root, home);
            if root == native {
                return Err(entry(format!(
                    "is the native {} root {}; consent for it is `enabled_kinds`, not `runtimes`",
                    adapter.kind(),
                    adapter.layout().data_root
                )));
            }
            if root.starts_with(&native) || native.starts_with(root) {
                return Err(entry(format!(
                    "is nested with the native {} root {}; one root may not be walked by two runtimes",
                    adapter.kind(),
                    native.display()
                )));
            }
            runtimes.push(EnrolledRuntime {
                adapter,
                runtime: RuntimeRow {
                    id: ids::runtime_id(adapter.kind(), &spec.data_root),
                    kind: adapter.kind().to_string(),
                    data_root: spec.data_root.clone(),
                    enabled: false,
                    account: None,
                    config: None,
                    skills: None,
                    versions_seen: None,
                    discovered_at: 0,
                    label: None,
                    provenance: Default::default(),
                },
                root: root.to_path_buf(),
                label: spec.label.clone(),
            });
        }
        Ok(Self {
            path: path.to_path_buf(),
            enabled_kinds: file.enabled_kinds,
            runtimes,
        })
    }

    /// The transport's decoration map: enrolled runtime id → label.
    pub fn presentation(&self) -> impl Iterator<Item = (String, Option<String>)> + '_ {
        self.runtimes
            .iter()
            .map(|runtime| (runtime.runtime.id.clone(), runtime.label.clone()))
    }

    pub fn enrolled_ids(&self) -> BTreeSet<String> {
        self.runtimes
            .iter()
            .map(|runtime| runtime.runtime.id.clone())
            .collect()
    }

    /// Map `enabled_kinds` onto the discovered targets: consent for every
    /// named kind that is known and installed. The rest are warnings the
    /// caller prints — a kind the supervisor names but this machine lacks is
    /// an ordinary state, never a refusal.
    pub fn enabled_discovered(&self, targets: &[RuntimeTarget]) -> (BTreeSet<String>, Vec<String>) {
        let mut ids = BTreeSet::new();
        let mut warnings = Vec::new();
        for kind in &self.enabled_kinds {
            match targets
                .iter()
                .find(|target| !target.is_enrolled() && target.kind() == kind)
            {
                Some(target) if target.installed() => {
                    ids.insert(target.id().to_string());
                }
                Some(_) => warnings.push(format!(
                    "--enroll {}: enabled kind {kind} is not installed; ignored",
                    self.path.display()
                )),
                None => warnings.push(format!(
                    "--enroll {}: enabled kind {kind} is not a registered (or selected) agent; ignored",
                    self.path.display()
                )),
            }
        }
        (ids, warnings)
    }
}

/// One runtime a serve mode iterates: the adapter, the identity row the
/// pipeline writes, the directory walked, and how it came to be enrolled.
/// Discovery yields one Discovered target per adapter as before; the file
/// adds Enrolled targets with an absolute `data_root`.
pub(crate) struct RuntimeTarget {
    pub adapter: &'static dyn AgentAdapter,
    pub runtime: RuntimeRow,
    pub root: PathBuf,
    pub label: Option<String>,
    pub provenance: RuntimeProvenance,
    pub census: Discovery,
}

impl RuntimeTarget {
    fn from_census(
        adapter: &'static dyn AgentAdapter,
        census: Discovery,
        label: Option<String>,
        provenance: RuntimeProvenance,
    ) -> Self {
        Self {
            adapter,
            runtime: census.runtime.clone(),
            root: census.root.clone(),
            label,
            provenance,
            census,
        }
    }

    pub fn id(&self) -> &str {
        &self.runtime.id
    }

    pub fn kind(&self) -> &'static str {
        self.adapter.kind()
    }

    pub fn is_enrolled(&self) -> bool {
        self.provenance == RuntimeProvenance::Enrolled
    }

    /// The census verdict: the root holds at least one file a layout stream
    /// claims.
    pub fn installed(&self) -> bool {
        self.census.installed
    }

    /// Whether a consented target is ingested and watched. A discovered root
    /// must be installed; an enrolled root is watched even while empty — the
    /// supervisor creates the directory before its agent writes a session,
    /// and the first one must appear live.
    pub fn active(&self) -> bool {
        self.is_enrolled() || self.installed()
    }

    pub fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity::new(self.id(), self.kind(), self.label.clone())
    }

    /// The log/diagnostic name: the kind alone for the discovered runtime,
    /// the kind with its label (or root) for an enrolled one.
    pub fn name(&self) -> String {
        runtime_name(
            self.kind(),
            self.label.as_deref(),
            self.is_enrolled().then_some(&self.runtime.data_root),
        )
    }

    /// The identity row the pipeline persists: consent flipped on. Labels and
    /// provenance never ride it — they are this start's file, not the
    /// projection's (and not the ledger's).
    pub fn enabled_runtime(&self) -> RuntimeRow {
        let mut runtime = self.runtime.clone();
        runtime.enabled = true;
        runtime
    }

    pub fn discovery_row(&self, enabled: bool) -> DiscoveryRow {
        DiscoveryRow {
            kind: self.kind().to_string(),
            data_root: self.runtime.data_root.clone(),
            installed: self.installed(),
            files: self.census.file_count as u64,
            sessions: self.census.session_count.map(|count| count as u64),
            bytes: self.census.total_bytes,
            newest_modified_ms: self.census.newest_modified_ms,
            enabled,
        }
    }
}

pub(crate) fn runtime_name(kind: &str, label: Option<&str>, root: Option<&String>) -> String {
    match (label, root) {
        (Some(label), _) => format!("{kind} «{label}»"),
        (None, Some(root)) => format!("{kind} ({root})"),
        (None, None) => kind.to_string(),
    }
}

/// Every runtime a command may serve: one Discovered target per selected
/// adapter (the layout's `~` root over `home`, id minted from the `~` form),
/// then one Enrolled target per file entry (absolute root, id minted from
/// it), each with the same census walk over its own root. The roots are
/// walked concurrently; targets keep that order.
pub(crate) fn resolve_targets(
    adapters: &[&'static dyn AgentAdapter],
    home: &Path,
    enrollment: Option<&Enrollment>,
    opts: &IngestOptions,
    instrument: &Instrument,
) -> Result<Vec<RuntimeTarget>, String> {
    let enrolled = enrollment.map_or(&[][..], |enrollment| &enrollment.runtimes[..]);
    let native_roots: Vec<PathBuf> = adapters
        .iter()
        .map(|adapter| expand_data_root(&adapter.layout().data_root, home))
        .collect();
    let mut roots = Vec::with_capacity(adapters.len() + enrolled.len());
    roots.extend(
        adapters
            .iter()
            .zip(&native_roots)
            .map(|(adapter, root)| CensusRoot {
                adapter: *adapter,
                data_root: &adapter.layout().data_root,
                root,
            }),
    );
    roots.extend(enrolled.iter().map(|enrolled| CensusRoot {
        adapter: enrolled.adapter,
        data_root: &enrolled.runtime.data_root,
        root: &enrolled.root,
    }));
    let mut censuses = discover_roots(&roots, opts, instrument)
        .map_err(|error| error.to_string())?
        .into_iter();

    let mut targets = Vec::with_capacity(roots.len());
    for (adapter, census) in adapters.iter().copied().zip(censuses.by_ref()) {
        targets.push(RuntimeTarget::from_census(
            adapter,
            census,
            None,
            RuntimeProvenance::Discovered,
        ));
    }
    for (enrolled, census) in enrolled.iter().zip(censuses) {
        debug_assert_eq!(census.runtime.id, enrolled.runtime.id);
        targets.push(RuntimeTarget::from_census(
            enrolled.adapter,
            census,
            enrolled.label.clone(),
            RuntimeProvenance::Enrolled,
        ));
    }
    Ok(targets)
}

/// The discovered target of each kind, for mapping kind-granular consent
/// (persisted flags, the roll call's grant, `--agent`) onto runtime ids.
pub(crate) fn discovered_by_kind(targets: &[RuntimeTarget]) -> BTreeMap<&str, &RuntimeTarget> {
    targets
        .iter()
        .filter(|target| !target.is_enrolled())
        .map(|target| (target.kind(), target))
        .collect()
}

#[cfg(test)]
#[path = "enroll_tests.rs"]
mod tests;
