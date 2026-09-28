//! Lossless codec between the closed Rust tool contract and canonical columns.
//! `tool_detail` is a serialized `ToolAction`, never a native request/result.

use std::error::Error;

use rosetta_core::{
    CorrelationDerivation, FailureKind, NativeToolRef, StatusBasis, ToolAction, ToolCorrelation,
    ToolFailure, ToolInteraction, ToolStatus, ToolStatusFact, ToolTiming,
};
use rusqlite::Row;
use rusqlite::types::Type;

use crate::StoreError;

crate::columns::column_map! {
    pub(crate) struct Columns {
        tool_correlation_key: "tool_correlation_key",
        tool_correlation_kind: "tool_correlation_kind",
        tool_name: "tool_name",
        tool_status: "tool_status",
        tool_status_basis: "tool_status_basis",
        tool_detail: "tool_detail",
        tool_error_kind: "tool_error_kind",
        tool_error_summary: "tool_error_summary",
        tool_diagnostic: "tool_diagnostic",
        tool_started_at: "tool_started_at",
        tool_completed_at: "tool_completed_at",
        tool_duration_ms: "tool_duration_ms",
        tool_native_id: "tool_native_id",
        tool_namespace: "tool_namespace",
        tool_provider: "tool_provider",
    }
}

#[derive(Debug, Default)]
pub(crate) struct EncodedTool<'a> {
    pub correlation_key: Option<&'a str>,
    pub correlation_kind: Option<&'static str>,
    pub native_id: Option<&'a str>,
    pub namespace: Option<&'a str>,
    pub provider: Option<&'a str>,
    pub name: Option<&'a str>,
    pub kind: Option<&'static str>,
    pub status: Option<&'static str>,
    pub status_basis: Option<&'static str>,
    pub detail: Option<String>,
    pub error_kind: Option<&'static str>,
    pub error_summary: Option<&'a str>,
    pub diagnostic: Option<&'a str>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
    pub duration_ms: Option<i64>,
}

pub(crate) fn encode(tool: Option<&ToolInteraction>) -> Result<EncodedTool<'_>, StoreError> {
    encode_with_detail(tool, None)
}

/// The action JSON [`encode`] stores in `tool_detail` — the one costly part of
/// encoding a tool row, which a producer that resolved the row can do off
/// the writer.
pub(crate) fn detail(tool: &ToolInteraction) -> Result<String, StoreError> {
    Ok(serde_json::to_string(&tool.action)?)
}

/// [`encode`], with `tool_detail` already serialized by [`detail`] when given.
pub(crate) fn encode_with_detail(
    tool: Option<&ToolInteraction>,
    detail: Option<String>,
) -> Result<EncodedTool<'_>, StoreError> {
    let Some(tool) = tool else {
        return Ok(EncodedTool::default());
    };
    let detail = match detail {
        Some(detail) => detail,
        None => self::detail(tool)?,
    };
    let duration_ms = tool
        .timing
        .and_then(|timing| timing.duration_ms)
        .map(i64::try_from)
        .transpose()
        .map_err(|_| StoreError::ProjectionInvalid {
            message: "tool duration exceeds SQLite integer range".into(),
        })?;
    Ok(EncodedTool {
        correlation_key: Some(&tool.correlation.key),
        correlation_kind: Some(correlation_name(tool.correlation.derivation)),
        native_id: tool.native.id.as_deref(),
        namespace: tool.native.namespace.as_deref(),
        provider: tool.native.provider.as_deref(),
        name: Some(&tool.native.name),
        kind: Some(tool.action.kind().as_str()),
        status: Some(status_name(tool.status.value)),
        status_basis: Some(status_basis_name(tool.status.basis)),
        detail: Some(detail),
        error_kind: tool
            .failure
            .as_ref()
            .map(|failure| failure_name(failure.kind)),
        error_summary: tool
            .failure
            .as_ref()
            .and_then(|failure| failure.summary.as_deref()),
        diagnostic: tool
            .failure
            .as_ref()
            .and_then(|failure| failure.diagnostic.as_deref()),
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

pub(crate) fn decode(row: &Row<'_>) -> rusqlite::Result<Option<ToolInteraction>> {
    decode_columns(row, &Columns::NAMED)
}

pub(crate) fn decode_columns<I: rusqlite::RowIndex + Copy>(
    row: &Row<'_>,
    columns: &Columns<I>,
) -> rusqlite::Result<Option<ToolInteraction>> {
    let correlation_key: Option<String> = row.get(columns.tool_correlation_key)?;
    let Some(correlation_key) = correlation_key else {
        return Ok(None);
    };
    let correlation_kind: String = row.get(columns.tool_correlation_kind)?;
    let name: String = row.get(columns.tool_name)?;
    let status: String = row.get(columns.tool_status)?;
    let status_basis: String = row.get(columns.tool_status_basis)?;
    let detail: String = row.get(columns.tool_detail)?;
    let action: ToolAction =
        serde_json::from_str(&detail).map_err(|error| decode_error(0, error))?;
    let failure_kind: Option<String> = row.get(columns.tool_error_kind)?;
    let failure = failure_kind
        .map(|kind| -> rusqlite::Result<ToolFailure> {
            Ok(ToolFailure {
                kind: parse_failure(&kind)?,
                summary: row.get(columns.tool_error_summary)?,
                diagnostic: row.get(columns.tool_diagnostic)?,
            })
        })
        .transpose()?;
    let started_at: Option<i64> = row.get(columns.tool_started_at)?;
    let completed_at: Option<i64> = row.get(columns.tool_completed_at)?;
    let duration_ms = row
        .get::<_, Option<i64>>(columns.tool_duration_ms)?
        .map(u64::try_from)
        .transpose()
        .map_err(|error| decode_error(0, error))?;
    let timing = (started_at.is_some() || completed_at.is_some() || duration_ms.is_some())
        .then_some(ToolTiming {
            started_at,
            completed_at,
            duration_ms,
        });
    Ok(Some(ToolInteraction {
        correlation: ToolCorrelation {
            key: correlation_key,
            derivation: parse_correlation(&correlation_kind)?,
        },
        native: NativeToolRef {
            id: row.get(columns.tool_native_id)?,
            namespace: row.get(columns.tool_namespace)?,
            name,
            provider: row.get(columns.tool_provider)?,
        },
        status: ToolStatusFact {
            value: parse_status(&status)?,
            basis: parse_status_basis(&status_basis)?,
        },
        action,
        failure,
        timing,
    }))
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
