//! Lossless codec between the closed Rust tool contract and schema-8 columns.
//! `tool_detail` is a serialized `ToolAction`, never a native request/result.

use std::error::Error;

use rosetta_core::{
    CorrelationDerivation, FailureKind, NativeToolRef, StatusBasis, ToolAction, ToolCorrelation,
    ToolFailure, ToolInteraction, ToolMergeState, ToolStatus, ToolStatusFact, ToolTiming,
};
use rusqlite::Row;
use rusqlite::types::Type;

use crate::StoreError;

#[derive(Debug, Default)]
pub(crate) struct EncodedTool {
    pub correlation_key: Option<String>,
    pub correlation_kind: Option<&'static str>,
    pub native_id: Option<String>,
    pub namespace: Option<String>,
    pub provider: Option<String>,
    pub name: Option<String>,
    pub kind: Option<&'static str>,
    pub status: Option<&'static str>,
    pub status_basis: Option<&'static str>,
    pub detail: Option<String>,
    pub merge_state: Option<String>,
    pub error_kind: Option<&'static str>,
    pub error_summary: Option<String>,
    pub diagnostic: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

pub(crate) fn encode(
    tool: Option<&ToolInteraction>,
    merge_state: Option<&ToolMergeState>,
) -> Result<EncodedTool, StoreError> {
    let Some(tool) = tool else {
        return Ok(EncodedTool::default());
    };
    let duration_ms = tool
        .timing
        .and_then(|timing| timing.duration_ms)
        .map(i64::try_from)
        .transpose()
        .map_err(|_| StoreError::ProjectionInvalid {
            message: "tool duration exceeds SQLite integer range".into(),
        })?;
    let empty_merge_state = ToolMergeState::default();
    Ok(EncodedTool {
        correlation_key: Some(tool.correlation.key.clone()),
        correlation_kind: Some(correlation_name(tool.correlation.derivation)),
        native_id: tool.native.id.clone(),
        namespace: tool.native.namespace.clone(),
        provider: tool.native.provider.clone(),
        name: Some(tool.native.name.clone()),
        kind: Some(tool.action.kind().as_str()),
        status: Some(status_name(tool.status.value)),
        status_basis: Some(status_basis_name(tool.status.basis)),
        detail: Some(serde_json::to_string(&tool.action)?),
        merge_state: Some(serde_json::to_string(
            merge_state.unwrap_or(&empty_merge_state),
        )?),
        error_kind: tool
            .failure
            .as_ref()
            .map(|failure| failure_name(failure.kind)),
        error_summary: tool
            .failure
            .as_ref()
            .and_then(|failure| failure.summary.clone()),
        diagnostic: tool
            .failure
            .as_ref()
            .and_then(|failure| failure.diagnostic.clone()),
        started_at: tool.timing.and_then(|timing| timing.started_at),
        completed_at: tool.timing.and_then(|timing| timing.completed_at),
        duration_ms,
    })
}

fn decode_error(index: usize, error: impl Error + Send + Sync + 'static) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(error))
}

fn required<T>(value: Option<T>, column: &'static str) -> rusqlite::Result<T> {
    value.ok_or_else(|| {
        decode_error(
            0,
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("tool row is missing {column}"),
            ),
        )
    })
}

pub(crate) fn decode(
    row: &Row<'_>,
) -> rusqlite::Result<(Option<ToolInteraction>, Option<ToolMergeState>)> {
    let correlation_key: Option<String> = row.get("tool_correlation_key")?;
    let Some(correlation_key) = correlation_key else {
        return Ok((None, None));
    };
    let correlation_kind: String = row.get("tool_correlation_kind")?;
    let name: String = row.get("tool_name")?;
    let status: String = row.get("tool_status")?;
    let status_basis: String = row.get("tool_status_basis")?;
    let detail: String = row.get("tool_detail")?;
    let merge_state: String = row.get("tool_merge_state")?;
    let action: ToolAction =
        serde_json::from_str(&detail).map_err(|error| decode_error(0, error))?;
    let merge_state: ToolMergeState =
        serde_json::from_str(&merge_state).map_err(|error| decode_error(0, error))?;
    let failure_kind: Option<String> = row.get("tool_error_kind")?;
    let failure = failure_kind
        .map(|kind| -> rusqlite::Result<ToolFailure> {
            Ok(ToolFailure {
                kind: parse_failure(&kind)?,
                summary: row.get("tool_error_summary")?,
                diagnostic: row.get("tool_diagnostic")?,
            })
        })
        .transpose()?;
    let started_at: Option<i64> = row.get("tool_started_at")?;
    let completed_at: Option<i64> = row.get("tool_completed_at")?;
    let duration_ms = row
        .get::<_, Option<i64>>("tool_duration_ms")?
        .map(u64::try_from)
        .transpose()
        .map_err(|error| decode_error(0, error))?;
    let timing = (started_at.is_some() || completed_at.is_some() || duration_ms.is_some())
        .then_some(ToolTiming {
            started_at,
            completed_at,
            duration_ms,
        });
    Ok((
        Some(ToolInteraction {
            correlation: ToolCorrelation {
                key: correlation_key,
                derivation: parse_correlation(&correlation_kind)?,
            },
            native: NativeToolRef {
                id: row.get("tool_native_id")?,
                namespace: row.get("tool_namespace")?,
                name,
                provider: row.get("tool_provider")?,
            },
            status: ToolStatusFact {
                value: parse_status(&status)?,
                basis: parse_status_basis(&status_basis)?,
            },
            action,
            failure,
            timing,
        }),
        Some(merge_state),
    ))
}

pub(crate) fn decode_open(row: &Row<'_>) -> rusqlite::Result<Option<ToolInteraction>> {
    row.get::<_, Option<String>>("tool_open")?
        .map(|json| serde_json::from_str(&json).map_err(|error| decode_error(0, error)))
        .transpose()
}

pub(crate) const fn correlation_name(value: CorrelationDerivation) -> &'static str {
    match value {
        CorrelationDerivation::NativeId => "native_id",
        CorrelationDerivation::ParentLinked => "parent_linked",
        CorrelationDerivation::Positional => "positional",
        CorrelationDerivation::Synthetic => "synthetic",
    }
}

pub(crate) const fn status_name(value: ToolStatus) -> &'static str {
    match value {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Succeeded => "succeeded",
        ToolStatus::Failed => "failed",
        ToolStatus::Denied => "denied",
        ToolStatus::Cancelled => "cancelled",
        ToolStatus::TimedOut => "timed_out",
        ToolStatus::Interrupted => "interrupted",
        ToolStatus::Unknown => "unknown",
    }
}

pub(crate) const fn status_basis_name(value: StatusBasis) -> &'static str {
    match value {
        StatusBasis::RequestObserved => "request_observed",
        StatusBasis::NativeState => "native_state",
        StatusBasis::NativeTerminalRecord => "native_terminal_record",
        StatusBasis::GroundTruthProtocol => "ground_truth_protocol",
        StatusBasis::SourceFinalization => "source_finalization",
    }
}

pub(crate) const fn failure_name(value: FailureKind) -> &'static str {
    match value {
        FailureKind::Permission => "permission",
        FailureKind::NotFound => "not_found",
        FailureKind::InvalidInput => "invalid_input",
        FailureKind::ExitNonZero => "exit_nonzero",
        FailureKind::Timeout => "timeout",
        FailureKind::Cancelled => "cancelled",
        FailureKind::Provider => "provider",
        FailureKind::Unknown => "unknown",
    }
}

fn parse_correlation(value: &str) -> rusqlite::Result<CorrelationDerivation> {
    required(
        match value {
            "native_id" => Some(CorrelationDerivation::NativeId),
            "parent_linked" => Some(CorrelationDerivation::ParentLinked),
            "positional" => Some(CorrelationDerivation::Positional),
            "synthetic" => Some(CorrelationDerivation::Synthetic),
            _ => None,
        },
        "valid tool_correlation_kind",
    )
}

fn parse_status(value: &str) -> rusqlite::Result<ToolStatus> {
    required(
        match value {
            "pending" => Some(ToolStatus::Pending),
            "running" => Some(ToolStatus::Running),
            "succeeded" => Some(ToolStatus::Succeeded),
            "failed" => Some(ToolStatus::Failed),
            "denied" => Some(ToolStatus::Denied),
            "cancelled" => Some(ToolStatus::Cancelled),
            "timed_out" => Some(ToolStatus::TimedOut),
            "interrupted" => Some(ToolStatus::Interrupted),
            "unknown" => Some(ToolStatus::Unknown),
            _ => None,
        },
        "valid tool_status",
    )
}

fn parse_status_basis(value: &str) -> rusqlite::Result<StatusBasis> {
    required(
        match value {
            "request_observed" => Some(StatusBasis::RequestObserved),
            "native_state" => Some(StatusBasis::NativeState),
            "native_terminal_record" => Some(StatusBasis::NativeTerminalRecord),
            "ground_truth_protocol" => Some(StatusBasis::GroundTruthProtocol),
            "source_finalization" => Some(StatusBasis::SourceFinalization),
            _ => None,
        },
        "valid tool_status_basis",
    )
}

fn parse_failure(value: &str) -> rusqlite::Result<FailureKind> {
    required(
        match value {
            "permission" => Some(FailureKind::Permission),
            "not_found" => Some(FailureKind::NotFound),
            "invalid_input" => Some(FailureKind::InvalidInput),
            "exit_nonzero" => Some(FailureKind::ExitNonZero),
            "timeout" => Some(FailureKind::Timeout),
            "cancelled" => Some(FailureKind::Cancelled),
            "provider" => Some(FailureKind::Provider),
            "unknown" => Some(FailureKind::Unknown),
            _ => None,
        },
        "valid tool_error_kind",
    )
}
