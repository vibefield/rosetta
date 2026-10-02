//! Shared mechanics for adapter-owned, versioned tool projector registries.
//!
//! The registrations live in each adapter.  This module supplies only the
//! closed selector/policy vocabulary, deterministic version matching, bounds,
//! and small constructors.  It never contains a vendor tool name or an
//! agent-specific JSON path.

use std::cmp::Ordering;

use rosetta_core::timeline::{
    CORE_SCALAR_MAX_BYTES, CanonicalError, CanonicalText, CorrelationDerivation,
    FAILURE_DIAGNOSTIC_MAX_BYTES, FAILURE_DIAGNOSTIC_MAX_LINES, ID_MAX_BYTES, NativeToolRef,
    ResourceRef, StatusBasis, ToolAction, ToolActionKind, ToolCorrelation, ToolFailure,
    ToolInteraction, ToolStatus, ToolStatusFact, ToolTiming, ToolUpdatePatch,
};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRange {
    pub min_inclusive: &'static str,
    pub max_inclusive: &'static str,
}

impl VersionRange {
    pub const fn exact(version: &'static str) -> Self {
        Self {
            min_inclusive: version,
            max_inclusive: version,
        }
    }

    pub fn contains(self, version: &str) -> bool {
        compare_versions(version, self.min_inclusive) != Ordering::Less
            && compare_versions(version, self.max_inclusive) != Ordering::Greater
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeToolSelector {
    pub namespace: Option<&'static str>,
    pub name: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusPolicy {
    /// The request opens pending; only a separate native lifecycle fact may
    /// establish a terminal state.
    ExplicitLifecycle,
    /// A terminal result record is ground-truth evidence of success unless it
    /// carries a typed native error fact.
    TerminalRecord,
    /// A native state enum is the sole status authority.
    NativeState,
    /// The writer protocol itself defines the observed completion record as
    /// terminal; this must be backed by the pinned ground truth.
    GroundTruthProtocol,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchPolicy {
    /// Use the canonical action's exhaustive search extractor.
    Canonical,
    /// Unknown actions index only their native name.
    NativeNameOnly,
    /// The reviewed action intentionally contributes no searchable facts.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectorRegistration {
    pub agent: &'static str,
    pub writer_versions: VersionRange,
    /// The native source format does not persist a writer version. This is an
    /// explicit evidence claim, never a fallback for an observed future
    /// version: `Some(version)` must still fall inside `writer_versions`.
    pub accepts_unversioned: bool,
    pub native: NativeToolSelector,
    pub action_kind: ToolActionKind,
    pub status_policy: StatusPolicy,
    pub search_policy: SearchPolicy,
    pub bounds_version: u32,
}

impl ProjectorRegistration {
    pub fn matches(
        &self,
        agent: &str,
        writer_version: Option<&str>,
        namespace: Option<&str>,
        name: &str,
    ) -> bool {
        self.agent == agent
            && writer_version.map_or(self.accepts_unversioned, |version| {
                self.writer_versions.contains(version)
            })
            && self.native.namespace == namespace
            && self.native.name == name
    }
}

pub fn lookup_registration<'a>(
    registrations: &'a [ProjectorRegistration],
    agent: &str,
    writer_version: Option<&str>,
    namespace: Option<&str>,
    name: &str,
) -> Result<&'a ProjectorRegistration, ProjectionError> {
    let registration = registrations
        .iter()
        .find(|registration| registration.matches(agent, writer_version, namespace, name));
    if let Some(registration) = registration {
        return Ok(registration);
    }
    let Some(writer_version) = writer_version else {
        return Err(ProjectionError::MissingWriterVersion);
    };
    Err(ProjectionError::Unregistered {
        agent: agent.to_owned(),
        writer_version: writer_version.to_owned(),
        namespace: namespace.map(str::to_owned),
        name: name.to_owned(),
    })
}

pub fn validate_registry(registrations: &[ProjectorRegistration]) -> Result<(), ProjectionError> {
    for (index, left) in registrations.iter().enumerate() {
        if left.bounds_version == 0 {
            return Err(ProjectionError::InvalidRegistration(format!(
                "{}:{} has bounds_version 0",
                left.agent, left.native.name
            )));
        }
        if compare_versions(
            left.writer_versions.min_inclusive,
            left.writer_versions.max_inclusive,
        ) == Ordering::Greater
        {
            return Err(ProjectionError::InvalidRegistration(format!(
                "{}:{} has an inverted writer range",
                left.agent, left.native.name
            )));
        }
        for right in &registrations[index + 1..] {
            if left.agent == right.agent
                && left.native == right.native
                && (ranges_overlap(left.writer_versions, right.writer_versions)
                    || (left.accepts_unversioned && right.accepts_unversioned))
            {
                return Err(ProjectionError::InvalidRegistration(format!(
                    "overlapping registrations for {}:{:?}:{}",
                    left.agent, left.native.namespace, left.native.name
                )));
            }
        }
    }
    Ok(())
}

fn ranges_overlap(left: VersionRange, right: VersionRange) -> bool {
    compare_versions(left.min_inclusive, right.max_inclusive) != Ordering::Greater
        && compare_versions(right.min_inclusive, left.max_inclusive) != Ordering::Greater
}

fn compare_versions(left: &str, right: &str) -> Ordering {
    fn pieces(version: &str) -> impl Iterator<Item = VersionPiece<'_>> {
        version
            .trim_start_matches(|character: char| !character.is_ascii_digit())
            .split(['.', '-', '+'])
            .filter(|piece| !piece.is_empty())
            .map(|piece| match piece.parse::<u64>() {
                Ok(number) => VersionPiece::Number(number),
                Err(_) => VersionPiece::Text(piece),
            })
    }

    let mut left = pieces(left);
    let mut right = pieces(right);
    loop {
        match (left.next(), right.next()) {
            (Some(a), Some(b)) => match a.cmp(&b) {
                Ordering::Equal => {}
                ordering => return ordering,
            },
            (Some(piece), None) => {
                return if piece.is_zero() {
                    left.find(|piece| !piece.is_zero())
                        .map_or(Ordering::Equal, |_| Ordering::Greater)
                } else {
                    Ordering::Greater
                };
            }
            (None, Some(piece)) => {
                return if piece.is_zero() {
                    right
                        .find(|piece| !piece.is_zero())
                        .map_or(Ordering::Equal, |_| Ordering::Less)
                } else {
                    Ordering::Less
                };
            }
            (None, None) => return Ordering::Equal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VersionPiece<'a> {
    Number(u64),
    Text(&'a str),
}

impl VersionPiece<'_> {
    fn is_zero(self) -> bool {
        matches!(self, Self::Number(0))
    }
}

impl Ord for VersionPiece<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Number(left), Self::Number(right)) => left.cmp(right),
            (Self::Text(left), Self::Text(right)) => left.cmp(right),
            // A release number sorts after an alphabetic prerelease piece.
            (Self::Number(_), Self::Text(_)) => Ordering::Greater,
            (Self::Text(_), Self::Number(_)) => Ordering::Less,
        }
    }
}

impl PartialOrd for VersionPiece<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    MissingWriterVersion,
    Unregistered {
        agent: String,
        writer_version: String,
        namespace: Option<String>,
        name: String,
    },
    MissingField(&'static str),
    InvalidField(&'static str),
    InvalidRegistration(String),
    Bounds(CanonicalError),
}

impl ProjectionError {
    /// Stable aggregate key for a projector refusal. The rejected native
    /// object stays only in the raw ledger; adapters combine this code with a
    /// privacy-safe structural shape and one ledger sample reference.
    pub const fn drift_code(&self) -> &'static str {
        match self {
            Self::MissingWriterVersion => "projection_missing_writer_version",
            Self::Unregistered { .. } => "projection_unregistered",
            Self::MissingField(_) => "projection_missing_field",
            Self::InvalidField(_) => "projection_invalid_field",
            Self::InvalidRegistration(_) => "projection_invalid_registration",
            Self::Bounds(_) => "projection_bounds",
        }
    }
}

pub fn projection_record_kind(phase: &str, namespace: Option<&str>, name: &str) -> String {
    let name = bounded_shape_component(name);
    namespace.map_or_else(
        || format!("tool/{phase}/{name}"),
        |namespace| {
            let namespace = bounded_shape_component(namespace);
            format!("tool/{phase}/{namespace}/{name}")
        },
    )
}

fn bounded_shape_component(value: &str) -> String {
    const PREFIX_BYTES: usize = 96;
    if value.len() <= PREFIX_BYTES {
        return value.to_owned();
    }
    let mut boundary = PREFIX_BYTES;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let digest = rosetta_core::ids::shape_hash(&[value.to_owned()]);
    format!("{}~{digest}", &value[..boundary])
}

/// Describe a rejected value by keys and JSON kinds only. Values never enter
/// drift aggregates, and traversal is deliberately bounded so malformed or
/// adversarial native payloads cannot create a second payload sink.
pub fn projection_shape_paths(value: &Value) -> Vec<String> {
    const MAX_PATHS: usize = 64;
    const MAX_DEPTH: usize = 6;

    fn kind(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "bool",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    fn walk(value: &Value, prefix: &str, depth: usize, out: &mut Vec<String>) {
        if out.len() >= MAX_PATHS {
            return;
        }
        if depth >= MAX_DEPTH {
            out.push(format!("{prefix}:depth_limit"));
            return;
        }
        match value {
            Value::Object(object) => {
                let mut keys: Vec<_> = object.keys().collect();
                keys.sort_unstable();
                for native_key in keys {
                    if out.len() >= MAX_PATHS {
                        break;
                    }
                    let child = &object[native_key];
                    let key = bounded_shape_component(native_key);
                    let path = if prefix.is_empty() {
                        key
                    } else {
                        format!("{prefix}.{key}")
                    };
                    out.push(format!("{path}:{}", kind(child)));
                    if matches!(child, Value::Object(_) | Value::Array(_)) {
                        walk(child, &path, depth + 1, out);
                    }
                }
            }
            Value::Array(values) => {
                out.push(format!("{prefix}[]:len_class_{}", values.len().min(20)));
                if let Some(first) = values.first() {
                    let path = format!("{prefix}[]");
                    out.push(format!("{path}:{}", kind(first)));
                    if matches!(first, Value::Object(_) | Value::Array(_)) {
                        walk(first, &path, depth + 1, out);
                    }
                }
            }
            scalar => out.push(format!("{prefix}:{}", kind(scalar))),
        }
    }

    let mut paths = Vec::new();
    walk(value, "$", 0, &mut paths);
    if paths.len() == MAX_PATHS {
        paths.push("$:path_limit".to_owned());
    }
    paths
}

impl std::fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingWriterVersion => formatter.write_str("tool record has no writer version"),
            Self::Unregistered {
                agent,
                writer_version,
                namespace,
                name,
            } => write!(
                formatter,
                "unregistered tool {agent}@{writer_version} {:?}:{name}",
                namespace
            ),
            Self::MissingField(field) => write!(formatter, "missing tool field {field}"),
            Self::InvalidField(field) => write!(formatter, "invalid tool field {field}"),
            Self::InvalidRegistration(message) => formatter.write_str(message),
            Self::Bounds(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ProjectionError {}

impl From<CanonicalError> for ProjectionError {
    fn from(value: CanonicalError) -> Self {
        Self::Bounds(value)
    }
}

pub fn canonical_text(value: impl Into<String>) -> Result<CanonicalText, ProjectionError> {
    // This is a normalizer-local draft. `IngestOp::project_message` walks the
    // closed action and externalizes any value above the inline limit before
    // validation, so user-critical commands/diffs/objectives are never turned
    // into an unknown action merely because they need a projection member.
    Ok(CanonicalText::Inline { text: value.into() })
}

pub fn bounded_scalar(
    field: &'static str,
    value: impl Into<String>,
) -> Result<String, ProjectionError> {
    let value = value.into();
    if value.len() > CORE_SCALAR_MAX_BYTES {
        return Err(ProjectionError::Bounds(CanonicalError::Overflow {
            field,
            bytes: value.len(),
            limit: CORE_SCALAR_MAX_BYTES,
        }));
    }
    Ok(value)
}

fn bounded_id(field: &'static str, value: impl Into<String>) -> Result<String, ProjectionError> {
    let value = value.into();
    if value.len() > ID_MAX_BYTES {
        return Err(ProjectionError::Bounds(CanonicalError::Overflow {
            field,
            bytes: value.len(),
            limit: ID_MAX_BYTES,
        }));
    }
    Ok(value)
}

pub fn file(path: impl Into<String>, resolved: bool) -> Result<ResourceRef, ProjectionError> {
    Ok(ResourceRef::File {
        path: bounded_scalar("file path", path)?,
        resolved,
    })
}

pub fn directory(path: impl Into<String>, resolved: bool) -> Result<ResourceRef, ProjectionError> {
    Ok(ResourceRef::Directory {
        path: bounded_scalar("directory path", path)?,
        resolved,
    })
}

pub fn url(value: impl Into<String>) -> Result<ResourceRef, ProjectionError> {
    Ok(ResourceRef::Url {
        url: bounded_scalar("URL", value)?,
    })
}

/// First non-empty string at one of the adapter-declared top-level keys.
/// The keys are supplied by the owning adapter; shared code never embeds a
/// vendor field name.
pub fn first_string<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    })
}

pub fn first_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_bool))
}

pub fn first_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|candidate| {
            candidate.as_u64().or_else(|| {
                candidate
                    .as_i64()
                    .and_then(|number| u64::try_from(number).ok())
            })
        })
    })
}

pub fn string_values(value: &Value, keys: &[&str], limit: usize) -> Vec<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_array))
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|text| !text.is_empty())
        .take(limit)
        .map(str::to_owned)
        .collect()
}

/// Resolve a source path lexically against the timestamped cwd.  This never
/// reads the live filesystem or follows symlinks.
pub fn lexical_resource(
    path: impl Into<String>,
    cwd: Option<&str>,
    is_directory: bool,
) -> Result<ResourceRef, ProjectionError> {
    let path = path.into();
    let portable = path.replace('\\', "/");
    let absolute = portable.starts_with('/')
        || portable
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':');
    let (path, resolved) = if absolute {
        (rosetta_core::ids::normalize_root(&portable), true)
    } else if let Some(cwd) = cwd.filter(|cwd| !cwd.is_empty()) {
        (
            rosetta_core::ids::normalize_root(&format!("{cwd}/{portable}")),
            true,
        )
    } else {
        (rosetta_core::ids::normalize_root(&portable), false)
    };
    if is_directory {
        directory(path, resolved)
    } else {
        file(path, resolved)
    }
}

pub fn open_interaction(
    correlation_key: impl Into<String>,
    correlation_derivation: CorrelationDerivation,
    native_id: Option<String>,
    namespace: Option<String>,
    native_name: impl Into<String>,
    provider: Option<String>,
    action: ToolAction,
) -> Result<ToolInteraction, ProjectionError> {
    Ok(ToolInteraction {
        attachments: Vec::new(),
        correlation: ToolCorrelation {
            key: bounded_id("tool correlation key", correlation_key)?,
            derivation: correlation_derivation,
        },
        native: NativeToolRef {
            id: native_id
                .map(|value| bounded_id("tool native id", value))
                .transpose()?,
            namespace: namespace
                .map(|value| bounded_id("tool namespace", value))
                .transpose()?,
            name: bounded_id("tool name", native_name)?,
            provider: provider
                .map(|value| bounded_id("tool provider", value))
                .transpose()?,
        },
        status: ToolStatusFact {
            value: ToolStatus::Pending,
            basis: StatusBasis::RequestObserved,
        },
        action,
        failure: None,
        timing: None,
    })
}

pub fn unknown_interaction(
    correlation_key: impl Into<String>,
    correlation_derivation: CorrelationDerivation,
    native_id: Option<String>,
    namespace: Option<String>,
    native_name: impl Into<String>,
    provider: Option<String>,
) -> ToolInteraction {
    let bounded = |value: String| bounded_shape_component(&value);
    let mut interaction = ToolInteraction::unknown(
        bounded(correlation_key.into()),
        correlation_derivation,
        bounded(native_name.into()),
    );
    interaction.native.id = native_id.map(bounded);
    interaction.native.namespace = namespace.map(bounded);
    interaction.native.provider = provider.map(bounded);
    interaction
}

/// Construct a terminal lifecycle patch without carrying successful output.
/// A failed action may keep one deterministic bounded diagnostic; callers
/// must pass only the source field selected by their reviewed projector.
pub fn terminal_update(
    is_error: Option<bool>,
    success_when_terminal: bool,
    basis: StatusBasis,
    at: Option<i64>,
    diagnostic: Option<&str>,
) -> ToolUpdatePatch {
    let value = match is_error {
        Some(true) => ToolStatus::Failed,
        Some(false) => ToolStatus::Succeeded,
        None if success_when_terminal => ToolStatus::Succeeded,
        None => ToolStatus::Unknown,
    };
    let failure = (value == ToolStatus::Failed).then(|| ToolFailure {
        kind: rosetta_core::timeline::FailureKind::Unknown,
        summary: None,
        diagnostic: diagnostic
            .map(bound_diagnostic)
            .filter(|text| !text.is_empty()),
    });
    ToolUpdatePatch {
        status: Some(ToolStatusFact { value, basis }),
        failure,
        timing: at.map(|completed_at| ToolTiming {
            started_at: None,
            completed_at: Some(completed_at),
            duration_ms: None,
        }),
        ..ToolUpdatePatch::default()
    }
}

fn bound_diagnostic(value: &str) -> String {
    let mut output = String::new();
    for line in value.lines().take(FAILURE_DIAGNOSTIC_MAX_LINES) {
        let separator = usize::from(!output.is_empty());
        let remaining = FAILURE_DIAGNOSTIC_MAX_BYTES.saturating_sub(output.len() + separator);
        if remaining == 0 {
            break;
        }
        if separator == 1 {
            output.push('\n');
        }
        if line.len() <= remaining {
            output.push_str(line);
            continue;
        }
        let mut boundary = remaining;
        while boundary > 0 && !line.is_char_boundary(boundary) {
            boundary -= 1;
        }
        output.push_str(&line[..boundary]);
        break;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY: &[ProjectorRegistration] = &[
        ProjectorRegistration {
            agent: "fixture",
            writer_versions: VersionRange {
                min_inclusive: "1.2.0",
                max_inclusive: "1.3.4",
            },
            accepts_unversioned: false,
            native: NativeToolSelector {
                namespace: None,
                name: "read",
            },
            action_kind: ToolActionKind::FileRead,
            status_policy: StatusPolicy::TerminalRecord,
            search_policy: SearchPolicy::Canonical,
            bounds_version: 1,
        },
        ProjectorRegistration {
            agent: "fixture",
            writer_versions: VersionRange::exact("1.3.4"),
            accepts_unversioned: false,
            native: NativeToolSelector {
                namespace: Some("mcp"),
                name: "read",
            },
            action_kind: ToolActionKind::External,
            status_policy: StatusPolicy::ExplicitLifecycle,
            search_policy: SearchPolicy::Canonical,
            bounds_version: 1,
        },
    ];

    #[test]
    fn version_ranges_are_numeric_not_lexical() {
        let range = VersionRange {
            min_inclusive: "0.99.0",
            max_inclusive: "0.150.1",
        };
        assert!(range.contains("0.100.0"));
        assert!(range.contains("rust-v0.150.1"));
        assert!(!range.contains("0.151.0"));
    }

    #[test]
    fn namespace_is_matched_before_name() {
        assert_eq!(
            lookup_registration(REGISTRY, "fixture", Some("1.3.4"), Some("mcp"), "read")
                .unwrap()
                .action_kind,
            ToolActionKind::External
        );
        assert_eq!(
            lookup_registration(REGISTRY, "fixture", Some("1.3.4"), None, "read")
                .unwrap()
                .action_kind,
            ToolActionKind::FileRead
        );
    }

    #[test]
    fn unknown_versions_fail_closed() {
        assert!(matches!(
            lookup_registration(REGISTRY, "fixture", Some("1.3.5"), None, "read"),
            Err(ProjectionError::Unregistered { .. })
        ));
    }

    #[test]
    fn unversioned_acceptance_is_explicit_and_never_accepts_a_future_version() {
        let registration = ProjectorRegistration {
            accepts_unversioned: true,
            ..REGISTRY[0]
        };
        assert!(registration.matches("fixture", None, None, "read"));
        assert!(!registration.matches("fixture", Some("1.3.5"), None, "read"));
        assert!(matches!(
            lookup_registration(REGISTRY, "fixture", None, None, "read"),
            Err(ProjectionError::MissingWriterVersion)
        ));
    }

    #[test]
    fn overlapping_selectors_are_rejected() {
        let duplicate = [REGISTRY[0], REGISTRY[0]];
        assert!(validate_registry(&duplicate).is_err());
        assert!(validate_registry(REGISTRY).is_ok());
    }

    #[test]
    fn projection_drift_shape_is_bounded_and_never_contains_values() {
        let mut value = serde_json::json!({
            "command": "private-command-value",
            "nested": {"token": "private-token-value"},
            "items": [{"path": "/private/path"}],
        });
        let object = value.as_object_mut().unwrap();
        for index in 0..100 {
            object.insert(format!("zz_extra_{index:03}"), Value::Bool(true));
        }
        let paths = projection_shape_paths(&value);
        assert!(paths.len() <= 65);
        assert!(paths.iter().any(|path| path == "$.command:string"));
        assert!(paths.iter().any(|path| path == "$.nested.token:string"));
        let debug = paths.join("\n");
        assert!(!debug.contains("private-command-value"));
        assert!(!debug.contains("private-token-value"));
        assert!(!debug.contains("/private/path"));
    }
}
