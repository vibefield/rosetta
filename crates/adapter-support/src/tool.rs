//! Shared mechanics for adapter-owned, versioned tool projector registries.
//!
//! The registrations live in each adapter.  This module supplies only the
//! closed selector/policy vocabulary, deterministic version matching, bounds,
//! and small constructors.  It never contains a vendor tool name or an
//! agent-specific JSON path.

use std::cmp::Ordering;

use rosetta_core::timeline::{
    CORE_SCALAR_MAX_BYTES, CanonicalError, CanonicalText, CorrelationDerivation, NativeToolRef,
    ResourceRef, StatusBasis, ToolAction, ToolActionKind, ToolCorrelation, ToolInteraction,
    ToolStatus, ToolStatusFact,
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
        writer_version: &str,
        namespace: Option<&str>,
        name: &str,
    ) -> bool {
        self.agent == agent
            && self.writer_versions.contains(writer_version)
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
    let Some(writer_version) = writer_version else {
        return Err(ProjectionError::MissingWriterVersion);
    };
    registrations
        .iter()
        .find(|registration| registration.matches(agent, writer_version, namespace, name))
        .ok_or_else(|| ProjectionError::Unregistered {
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
                && ranges_overlap(left.writer_versions, right.writer_versions)
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
    Ok(CanonicalText::inline(value)?)
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
) -> ToolInteraction {
    ToolInteraction {
        correlation: ToolCorrelation {
            key: correlation_key.into(),
            derivation: correlation_derivation,
        },
        native: NativeToolRef {
            id: native_id,
            namespace,
            name: native_name.into(),
            provider,
        },
        status: ToolStatusFact {
            value: ToolStatus::Pending,
            basis: StatusBasis::RequestObserved,
        },
        action,
        failure: None,
        timing: None,
    }
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
    fn overlapping_selectors_are_rejected() {
        let duplicate = [REGISTRY[0], REGISTRY[0]];
        assert!(validate_registry(&duplicate).is_err());
        assert!(validate_registry(REGISTRY).is_ok());
    }
}
