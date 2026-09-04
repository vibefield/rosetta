//! One light, session-scoped index over work already present in the canonical
//! projection. The transcript remains the record: this query returns anchors
//! and safe open references, never a second copy of task, plan, or artifact
//! bodies.

use std::collections::BTreeMap;

use rosetta_core::rows::{Ms, TaskStatus};
use rosetta_core::timeline::{
    ArtifactKind, ArtifactOp, AssetRef, PlanOp, ResourceRef, TaskSnapshotItem, TimelineBody,
    ToolAction,
};
use rosetta_core::{TimelineEntry, ids};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::{StoreError, message_codec};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelineMeta {
    pub tasks: Vec<TimelineMetaTask>,
    pub plans: Vec<TimelineMetaPlan>,
    pub artifacts: Vec<TimelineMetaArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelineMetaTask {
    pub id: String,
    pub subject: String,
    pub status: Option<TaskStatus>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub started_at: Option<Ms>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub terminal_at: Option<Ms>,
    /// Exact task-family row to land on. `None` is honest for native task
    /// tables that have no corresponding transcript act.
    pub timeline_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelineMetaPlan {
    pub id: String,
    pub name: String,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub updated_at: Option<Ms>,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub byte_len: u64,
    pub source: TimelineMetaPlanSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum TimelineMetaPlanSource {
    Revision {
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        revision: i64,
    },
    Timeline {
        timeline_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct TimelineMetaArtifact {
    pub id: String,
    pub name: String,
    pub artifact_kind: ArtifactKind,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub created_at: Option<Ms>,
    #[cfg_attr(feature = "ts", ts(type = "number | null"))]
    pub byte_len: Option<u64>,
    pub media_type: Option<String>,
    pub source: TimelineMetaArtifactSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub enum TimelineMetaArtifactSource {
    TimelineFile {
        timeline_id: String,
        #[cfg_attr(feature = "ts", ts(type = "number"))]
        target_index: u64,
        path: String,
    },
    Asset {
        asset: AssetRef,
    },
}

/// A plan body prepared only after an explicit open action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDocument {
    pub name: String,
    pub content: String,
}

#[derive(Debug)]
struct StoredTask {
    item: TimelineMetaTask,
    order: i64,
}

#[derive(Debug, Default)]
struct TaskTrace {
    subject: Option<String>,
    status: Option<TaskStatus>,
    started_at: Option<Ms>,
    terminal_at: Option<Ms>,
    timeline_id: Option<String>,
}

impl TaskTrace {
    fn observe(
        &mut self,
        subject: Option<&str>,
        status: Option<TaskStatus>,
        at: Option<Ms>,
        timeline_id: &str,
    ) {
        if let Some(subject) = subject.filter(|value| !value.trim().is_empty()) {
            self.subject = Some(subject.to_owned());
        }
        if let Some(status) = status {
            if status == TaskStatus::Pending {
                self.started_at = None;
                self.terminal_at = None;
            } else if status == TaskStatus::InProgress
                && self.status != Some(TaskStatus::InProgress)
            {
                self.started_at = at;
                self.terminal_at = None;
            } else if is_terminal(Some(status)) && !is_terminal(self.status) {
                self.terminal_at = at;
            }
            self.status = Some(status);
        }
        self.timeline_id = Some(timeline_id.to_owned());
    }
}

fn is_terminal(status: Option<TaskStatus>) -> bool {
    matches!(
        status,
        Some(TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled)
    )
}

fn parse_task_status(value: &str) -> Option<TaskStatus> {
    match value {
        "pending" => Some(TaskStatus::Pending),
        "in_progress" => Some(TaskStatus::InProgress),
        "completed" => Some(TaskStatus::Completed),
        "failed" => Some(TaskStatus::Failed),
        "cancelled" => Some(TaskStatus::Cancelled),
        _ => None,
    }
}

fn tool_entries(conn: &Connection, session_id: &str) -> Result<Vec<TimelineEntry>, StoreError> {
    let mut statement = conn.prepare_cached(
        "SELECT m.* FROM message m
         WHERE m.session_id = ?1 AND m.tool_kind IN ('task', 'plan', 'artifact')
         ORDER BY m.ordinal, m.id",
    )?;
    let rows = statement.query_map(params![session_id], message_codec::decode)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

fn stored_tasks(
    conn: &Connection,
    session_id: &str,
) -> Result<BTreeMap<String, StoredTask>, StoreError> {
    let mut statement = conn.prepare_cached(
        "SELECT id, status, content, started_at, terminal_at,
                COALESCE(created_at, updated_at, 9223372036854775807) AS task_order
         FROM task WHERE session_id = ?1
         ORDER BY task_order, id",
    )?;
    let rows = statement.query_map(params![session_id], |row| {
        let status: String = row.get(1)?;
        Ok((
            row.get::<_, String>(0)?,
            status,
            row.get::<_, String>(2)?,
            row.get::<_, Option<Ms>>(3)?,
            row.get::<_, Option<Ms>>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let mut tasks = BTreeMap::new();
    for row in rows {
        let (id, status, subject, started_at, terminal_at, order) = row?;
        tasks.insert(
            id.clone(),
            StoredTask {
                item: TimelineMetaTask {
                    id,
                    subject,
                    status: parse_task_status(&status),
                    started_at,
                    terminal_at,
                    timeline_id: None,
                },
                order,
            },
        );
    }
    Ok(tasks)
}

fn task_meta(
    conn: &Connection,
    session_id: &str,
    entries: &[TimelineEntry],
) -> Result<Vec<TimelineMetaTask>, StoreError> {
    let mut tasks = stored_tasks(conn, session_id)?;
    let mut traces: BTreeMap<String, TaskTrace> = BTreeMap::new();
    let mut candidates: BTreeMap<String, StoredTask> = BTreeMap::new();
    let mut latest_snapshot: Option<(i64, String, Vec<TaskSnapshotItem>)> = None;

    for entry in entries {
        let TimelineBody::ToolUse { tool } = &entry.body else {
            continue;
        };
        let ToolAction::Task {
            task_id,
            subject,
            status,
            snapshot,
            ..
        } = &tool.action
        else {
            continue;
        };
        let event_at = tool
            .timing
            .and_then(|timing| timing.completed_at.or(timing.started_at))
            .or(entry.at);
        if let Some(native_id) = task_id {
            let id = ids::task_id(session_id, native_id);
            traces.entry(id.clone()).or_default().observe(
                subject.as_deref(),
                *status,
                event_at,
                &entry.timeline_id,
            );
            let candidate = candidates.entry(id.clone()).or_insert_with(|| StoredTask {
                item: TimelineMetaTask {
                    id,
                    subject: subject.clone().unwrap_or_else(|| native_id.clone()),
                    status: *status,
                    started_at: None,
                    terminal_at: None,
                    timeline_id: Some(entry.timeline_id.clone()),
                },
                order: entry.ordinal,
            });
            if let Some(subject) = subject.as_ref().filter(|value| !value.trim().is_empty()) {
                candidate.item.subject = subject.clone();
            }
            if status.is_some() {
                candidate.item.status = *status;
            }
            candidate.item.timeline_id = Some(entry.timeline_id.clone());
        }
        if let Some(snapshot) = snapshot {
            latest_snapshot = Some((
                entry.ordinal,
                entry.timeline_id.clone(),
                snapshot.tasks.clone(),
            ));
            for task in &snapshot.tasks {
                let id = ids::task_id(session_id, &task.task_id);
                traces.entry(id).or_default().observe(
                    task.subject.as_deref(),
                    task.status,
                    event_at,
                    &entry.timeline_id,
                );
            }
        }
    }

    // A point-in-time board may be deliberately bounded. Keep every task
    // that had its own canonical action as well as the latest board rows, so
    // the work index never drops an older or out-of-window task we can prove.
    for (id, candidate) in candidates {
        tasks.entry(id).or_insert(candidate);
    }

    if let Some((order, timeline_id, snapshot)) = latest_snapshot {
        for row in snapshot {
            let id = ids::task_id(session_id, &row.task_id);
            let task = tasks.entry(id.clone()).or_insert_with(|| StoredTask {
                item: TimelineMetaTask {
                    id,
                    subject: row.subject.clone().unwrap_or_else(|| row.task_id.clone()),
                    status: row.status,
                    started_at: None,
                    terminal_at: None,
                    timeline_id: Some(timeline_id.clone()),
                },
                order,
            });
            if let Some(subject) = row.subject.filter(|value| !value.trim().is_empty()) {
                task.item.subject = subject;
            }
            if row.status.is_some() {
                task.item.status = row.status;
            }
            task.item.timeline_id = Some(timeline_id.clone());
        }
    }

    for (id, task) in &mut tasks {
        let Some(trace) = traces.get(id) else {
            continue;
        };
        if task.item.started_at.is_none() {
            task.item.started_at = trace.started_at;
        }
        if task.item.terminal_at.is_none() && is_terminal(task.item.status) {
            task.item.terminal_at = trace.terminal_at;
        }
        if trace.timeline_id.is_some() {
            task.item.timeline_id.clone_from(&trace.timeline_id);
        }
    }

    let mut ordered: Vec<StoredTask> = tasks.into_values().collect();
    ordered.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.item.subject.cmp(&right.item.subject))
            .then_with(|| left.item.id.cmp(&right.item.id))
    });
    Ok(ordered.into_iter().map(|task| task.item).collect())
}

fn plan_filename(title: Option<&str>, fallback: &str) -> String {
    let raw = title
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback);
    let mut stem: String = raw
        .chars()
        .take(80)
        .map(|character| match character {
            '/' | '\\' | ':' => '-',
            _ => character,
        })
        .collect();
    if !stem.to_ascii_lowercase().ends_with(".md") {
        stem.push_str(".md");
    }
    stem
}

fn render_timeline_plan(action: &ToolAction) -> Option<PlanDocument> {
    let ToolAction::Plan {
        operation,
        plan_id,
        title,
        summary,
        step_count,
    } = action
    else {
        return None;
    };
    if *operation == PlanOp::Clear {
        return None;
    }
    let heading = title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Session plan");
    let mut content = format!("# {heading}\n");
    if let Some(summary) = summary {
        content.push('\n');
        content.push_str(summary.as_preview());
        content.push('\n');
    }
    if let Some(step_count) = step_count {
        content.push_str(&format!("\nSteps recorded: {step_count}\n"));
    }
    if let Some(plan_id) = plan_id {
        content.push_str(&format!("\nPlan id: `{plan_id}`\n"));
    }
    Some(PlanDocument {
        name: plan_filename(title.as_deref(), "plan-summary"),
        content,
    })
}

fn plan_meta(
    conn: &Connection,
    session_id: &str,
    entries: &[TimelineEntry],
) -> Result<Vec<TimelineMetaPlan>, StoreError> {
    let stored = conn
        .prepare_cached(
            "SELECT revision,
                    length(CAST(content AS BLOB)),
                    substr(ltrim(content), 1, 1),
                    updated_at
             FROM plan
             WHERE session_id = ?1 ORDER BY revision DESC LIMIT 1",
        )?
        .query_row(params![session_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Ms>(3)?,
            ))
        })
        .optional()?;

    let mut plans = Vec::new();
    if let Some((revision, byte_len, first_character, updated_at)) = stored {
        let byte_len = u64::try_from(byte_len).map_err(|_| StoreError::ProjectionInvalid {
            message: format!("plan revision {revision} has a negative byte length"),
        })?;
        let extension = if matches!(first_character.as_str(), "{" | "[") {
            "json"
        } else {
            "md"
        };
        plans.push(TimelineMetaPlan {
            id: format!("revision:{revision}"),
            name: format!("plan-r{revision}.{extension}"),
            updated_at: Some(updated_at),
            byte_len,
            source: TimelineMetaPlanSource::Revision { revision },
        });
    }

    let mut timeline_plans: BTreeMap<String, TimelineMetaPlan> = BTreeMap::new();
    for entry in entries {
        let TimelineBody::ToolUse { tool } = &entry.body else {
            continue;
        };
        let ToolAction::Plan {
            operation, plan_id, ..
        } = &tool.action
        else {
            continue;
        };
        let key = plan_id.as_deref().unwrap_or("session-plan").to_owned();
        if *operation == PlanOp::Clear {
            timeline_plans.remove(&key);
            continue;
        }
        let Some(document) = render_timeline_plan(&tool.action) else {
            continue;
        };
        let observed_at = tool
            .timing
            .and_then(|timing| timing.completed_at.or(timing.started_at))
            .or(entry.at);
        let updated_at = match operation {
            PlanOp::Create | PlanOp::Update => observed_at,
            PlanOp::Show => timeline_plans.get(&key).and_then(|plan| plan.updated_at),
            PlanOp::Clear => unreachable!("clear operations are handled above"),
        };
        timeline_plans.insert(
            key.clone(),
            TimelineMetaPlan {
                id: format!("timeline:{key}"),
                name: document.name,
                updated_at,
                byte_len: u64::try_from(document.content.len()).unwrap_or(u64::MAX),
                source: TimelineMetaPlanSource::Timeline {
                    timeline_id: entry.timeline_id.clone(),
                },
            },
        );
    }
    plans.extend(timeline_plans.into_values());
    plans.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(plans)
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

fn asset_name(asset: &AssetRef) -> String {
    asset
        .url
        .rsplit('/')
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(&asset.digest)
        .to_owned()
}

fn artifact_meta(entries: &[TimelineEntry]) -> Vec<TimelineMetaArtifact> {
    let mut artifacts: BTreeMap<String, TimelineMetaArtifact> = BTreeMap::new();
    for entry in entries {
        let TimelineBody::ToolUse { tool } = &entry.body else {
            continue;
        };
        let ToolAction::Artifact {
            operation,
            artifact_kind,
            name,
            resources,
            media,
            ..
        } = &tool.action
        else {
            continue;
        };
        let created_at = tool
            .timing
            .and_then(|timing| timing.completed_at.or(timing.started_at))
            .or(entry.at);
        let one_resource = resources.len() == 1;
        for (target_index, resource) in resources.iter().enumerate() {
            let (identity, fallback_name, byte_len, media_type, source) = match resource {
                ResourceRef::File { path, .. } => (
                    format!("file:{path}"),
                    basename(path).to_owned(),
                    None,
                    media.as_ref().and_then(|value| value.media_type.clone()),
                    TimelineMetaArtifactSource::TimelineFile {
                        timeline_id: entry.timeline_id.clone(),
                        target_index: u64::try_from(target_index).unwrap_or(u64::MAX),
                        path: path.clone(),
                    },
                ),
                ResourceRef::Asset { asset } => (
                    format!("asset:{}", asset.digest),
                    asset_name(asset),
                    Some(asset.byte_len),
                    Some(asset.media_type.clone()),
                    TimelineMetaArtifactSource::Asset {
                        asset: asset.clone(),
                    },
                ),
                ResourceRef::Directory { .. }
                | ResourceRef::Url { .. }
                | ResourceRef::Session { .. }
                | ResourceRef::Task { .. }
                | ResourceRef::Process { .. } => continue,
            };
            let display_name = if one_resource {
                name.as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .unwrap_or(&fallback_name)
                    .to_owned()
            } else {
                fallback_name
            };
            let created_at = if *operation == ArtifactOp::Create {
                artifacts
                    .get(&identity)
                    .and_then(|artifact| artifact.created_at)
                    .or(created_at)
            } else {
                artifacts
                    .get(&identity)
                    .and_then(|artifact| artifact.created_at)
            };
            artifacts.insert(
                identity.clone(),
                TimelineMetaArtifact {
                    id: identity,
                    name: display_name,
                    artifact_kind: *artifact_kind,
                    created_at,
                    byte_len,
                    media_type,
                    source,
                },
            );
        }
    }
    let mut artifacts: Vec<_> = artifacts.into_values().collect();
    artifacts.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| left.name.cmp(&right.name))
    });
    artifacts
}

/// Read all three indexes through one Rust boundary. SQLite performs one
/// indexed task lookup, one latest-plan lookup, and one narrow tool-kind scan;
/// file and plan bodies remain lazy.
pub fn read(conn: &Connection, session_id: &str) -> Result<TimelineMeta, StoreError> {
    let entries = tool_entries(conn, session_id)?;
    Ok(TimelineMeta {
        tasks: task_meta(conn, session_id, &entries)?,
        plans: plan_meta(conn, session_id, &entries)?,
        artifacts: artifact_meta(&entries),
    })
}

pub fn revision_plan_document(
    conn: &Connection,
    session_id: &str,
    revision: i64,
) -> Result<Option<PlanDocument>, StoreError> {
    conn.prepare_cached("SELECT content FROM plan WHERE session_id = ?1 AND revision = ?2")?
        .query_row(params![session_id, revision], |row| row.get::<_, String>(0))
        .optional()
        .map(|content| {
            content.map(|content| {
                let extension = if serde_json::from_str::<serde_json::Value>(&content).is_ok() {
                    "json"
                } else {
                    "md"
                };
                PlanDocument {
                    name: format!("plan-r{revision}.{extension}"),
                    content,
                }
            })
        })
        .map_err(StoreError::from)
}

pub fn timeline_plan_document(
    conn: &Connection,
    session_id: &str,
    timeline_id: &str,
) -> Result<Option<PlanDocument>, StoreError> {
    let entry = conn
        .prepare_cached(
            "SELECT m.* FROM message m
             WHERE m.session_id = ?1 AND m.timeline_id = ?2 AND m.tool_kind = 'plan'",
        )?
        .query_row(params![session_id, timeline_id], message_codec::decode)
        .optional()?;
    Ok(entry.and_then(|entry| match entry.body {
        TimelineBody::ToolUse { tool } => render_timeline_plan(&tool.action),
        _ => None,
    }))
}
