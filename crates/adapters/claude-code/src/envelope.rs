//! Claude's XML-shaped delivery wrappers → agent-neutral collaboration facts.
//!
//! Only complete top-level wrappers are recognized.  A wrapper nested inside
//! a task/result body is transport provenance and must never become a second
//! chronological message.

use std::collections::BTreeMap;

use rosetta_core::{
    CollaborationChannel, CollaborationEventKind, CollaborationStatus, Ms, ParticipantColor,
};
use serde_json::Value;

use crate::raw::transcript::UserOrigin;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CollaborationDraft {
    pub kind: CollaborationEventKind,
    pub channel: CollaborationChannel,
    pub sender: Option<String>,
    pub recipient: Option<String>,
    pub team: Option<String>,
    pub color: Option<ParticipantColor>,
    pub summary: Option<String>,
    pub body: Option<String>,
    pub task_id: Option<String>,
    pub delivery_id: Option<String>,
    pub status: Option<CollaborationStatus>,
    /// Timestamp authored inside the payload. The outer record remains the
    /// observation time used to place the row in its transcript.
    pub authored_at: Option<Ms>,
}

/// Exact fields carried by Claude's top-level async completion callback. The
/// output-file path is intentionally not retained: it is an ephemeral cache,
/// while `result` plus the Workflow run sidecar are durable evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskNotificationDraft {
    pub task_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub status: Option<String>,
    pub summary: Option<String>,
    pub result: Option<String>,
    pub agent_count: Option<u64>,
    pub tokens: Option<u64>,
    pub tool_uses: Option<u64>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Envelope<'a> {
    name: &'a str,
    attributes: BTreeMap<String, String>,
    body: &'a str,
}

/// `Some` means the record is machine-delivered and must not become a human
/// prompt. An empty vector is a task notification handled by the lifecycle
/// projector before this general collaboration classifier runs.
pub(crate) fn classify(
    text: &str,
    origin: Option<&UserOrigin>,
    team_name: Option<&str>,
    child_stream: bool,
) -> Option<Vec<CollaborationDraft>> {
    if let Some(envelopes) = top_level_envelopes(text) {
        let mut drafts = Vec::new();
        for envelope in envelopes {
            match envelope.name {
                "agent-message" => drafts.push(agent_message(envelope, origin)),
                "teammate-message" => drafts.push(teammate_message(envelope, team_name)),
                // Async callbacks share this wrapper across Workflow, Agent,
                // shell, and other tools. The lifecycle projector owns them.
                "task-notification" => {}
                _ => unreachable!("top_level_envelopes is a closed recognizer"),
            }
        }
        return Some(drafts);
    }

    match origin.map(|origin| origin.kind.as_str()) {
        Some("peer") => Some(vec![CollaborationDraft {
            kind: CollaborationEventKind::Message,
            channel: CollaborationChannel::Peer,
            sender: origin.and_then(peer_name),
            recipient: None,
            team: team_name.map(str::to_owned),
            color: None,
            summary: None,
            body: origin
                .and_then(|origin| nonempty(origin.body.as_deref()))
                .or_else(|| nonempty(Some(text)))
                .map(str::to_owned),
            task_id: origin.and_then(|origin| origin.sender_task_id.clone()),
            delivery_id: None,
            status: None,
            authored_at: None,
        }]),
        Some("coordinator") => Some(vec![CollaborationDraft {
            kind: CollaborationEventKind::Message,
            channel: CollaborationChannel::Coordinator,
            sender: Some("coordinator".to_owned()),
            recipient: None,
            team: team_name.map(str::to_owned),
            color: None,
            summary: None,
            body: nonempty(Some(text)).map(str::to_owned),
            task_id: None,
            delivery_id: None,
            status: None,
            authored_at: None,
        }]),
        Some("task-notification") => Some(Vec::new()),
        Some("human") => None,
        // A direct child transcript is owned by its parent invocation.  Its
        // untagged prose opener is an assignment, not a human-authored prompt.
        None if child_stream => Some(vec![CollaborationDraft {
            kind: CollaborationEventKind::Assignment,
            channel: CollaborationChannel::Coordinator,
            sender: Some("coordinator".to_owned()),
            recipient: None,
            team: team_name.map(str::to_owned),
            color: None,
            summary: None,
            body: nonempty(Some(text)).map(str::to_owned),
            task_id: None,
            delivery_id: None,
            status: None,
            authored_at: None,
        }]),
        _ => None,
    }
}

/// Parse only one complete top-level `<task-notification>`. A nested copy is
/// deliberately invisible to this projector, matching [`classify`]'s
/// ownership rule.
pub(crate) fn task_notification(text: &str) -> Option<TaskNotificationDraft> {
    let mut envelopes = top_level_envelopes(text)?;
    if envelopes.len() != 1 || envelopes[0].name != "task-notification" {
        return None;
    }
    let envelope = envelopes.remove(0);
    let usage = element(envelope.body, "usage").unwrap_or_default();
    Some(TaskNotificationDraft {
        task_id: owned_element(envelope.body, "task-id"),
        tool_use_id: owned_element(envelope.body, "tool-use-id"),
        status: owned_element(envelope.body, "status"),
        summary: owned_element(envelope.body, "summary"),
        result: owned_element(envelope.body, "result"),
        agent_count: number_element(usage, "agent_count"),
        tokens: number_element(usage, "subagent_tokens"),
        tool_uses: number_element(usage, "tool_uses"),
        duration_ms: number_element(usage, "duration_ms"),
    })
}

fn owned_element(input: &str, name: &str) -> Option<String> {
    nonempty(element(input, name)).map(str::to_owned)
}

fn number_element(input: &str, name: &str) -> Option<u64> {
    nonempty(element(input, name))?.parse().ok()
}

fn element<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let opening = format!("<{name}>");
    let closing = format!("</{name}>");
    let start = input.find(&opening)? + opening.len();
    let end = input[start..].find(&closing)? + start;
    Some(&input[start..end])
}

fn agent_message(envelope: Envelope<'_>, origin: Option<&UserOrigin>) -> CollaborationDraft {
    CollaborationDraft {
        kind: CollaborationEventKind::Message,
        channel: CollaborationChannel::Peer,
        sender: origin
            .and_then(peer_name)
            .or_else(|| envelope.attributes.get("from").cloned()),
        recipient: None,
        team: None,
        color: None,
        summary: None,
        // Structured origin carries the unwrapped body on newer writers and
        // is the stronger representation when both are present.
        body: origin
            .and_then(|origin| nonempty(origin.body.as_deref()))
            .or_else(|| nonempty(Some(envelope.body)))
            .map(str::to_owned),
        task_id: origin.and_then(|origin| origin.sender_task_id.clone()),
        delivery_id: None,
        status: None,
        authored_at: None,
    }
}

fn teammate_message(envelope: Envelope<'_>, team_name: Option<&str>) -> CollaborationDraft {
    let sender = envelope.attributes.get("teammate_id").cloned();
    let color = envelope
        .attributes
        .get("color")
        .and_then(|value| participant_color(value));
    let wrapper_summary = envelope.attributes.get("summary").cloned();
    let raw_body = envelope.body.trim();
    let payload = serde_json::from_str::<Value>(raw_body)
        .ok()
        .filter(Value::is_object);

    let payload_type = payload.as_ref().and_then(|value| value["type"].as_str());
    let authored_at = string_ref(&payload, "timestamp").and_then(crate::time::rfc3339_ms);
    match payload_type {
        Some("idle_notification") => CollaborationDraft {
            kind: CollaborationEventKind::Idle,
            channel: CollaborationChannel::Team,
            sender: string(&payload, "from").or(sender),
            recipient: None,
            team: team_name.map(str::to_owned),
            color,
            summary: string(&payload, "summary").or(wrapper_summary),
            body: None,
            task_id: None,
            delivery_id: None,
            status: match string_ref(&payload, "idleReason") {
                Some("available") => Some(CollaborationStatus::Available),
                Some("failed") => Some(CollaborationStatus::Failed),
                _ => None,
            },
            authored_at,
        },
        Some("task_assignment") => CollaborationDraft {
            kind: CollaborationEventKind::Assignment,
            channel: CollaborationChannel::Team,
            sender: string(&payload, "assignedBy"),
            recipient: sender,
            team: team_name.map(str::to_owned),
            color,
            summary: string(&payload, "subject").or(wrapper_summary),
            body: string(&payload, "description"),
            task_id: string(&payload, "taskId"),
            delivery_id: None,
            status: None,
            authored_at,
        },
        Some("shutdown_request") => CollaborationDraft {
            kind: CollaborationEventKind::StopRequested,
            channel: CollaborationChannel::Team,
            sender: string(&payload, "from").or(sender),
            recipient: None,
            team: team_name.map(str::to_owned),
            color,
            summary: wrapper_summary,
            body: string(&payload, "reason"),
            task_id: None,
            delivery_id: string(&payload, "requestId"),
            status: None,
            authored_at,
        },
        Some("shutdown_approved") => CollaborationDraft {
            kind: CollaborationEventKind::StopApproved,
            channel: CollaborationChannel::Team,
            sender: string(&payload, "from").or(sender),
            recipient: None,
            team: team_name.map(str::to_owned),
            color,
            summary: wrapper_summary,
            body: None,
            task_id: None,
            delivery_id: string(&payload, "requestId"),
            status: None,
            authored_at,
        },
        Some("teammate_terminated") => CollaborationDraft {
            kind: CollaborationEventKind::Terminated,
            channel: CollaborationChannel::Team,
            sender,
            recipient: None,
            team: team_name.map(str::to_owned),
            color,
            summary: wrapper_summary,
            body: string(&payload, "message"),
            task_id: None,
            delivery_id: None,
            status: Some(CollaborationStatus::Completed),
            authored_at,
        },
        _ => CollaborationDraft {
            kind: CollaborationEventKind::Message,
            channel: CollaborationChannel::Team,
            sender,
            recipient: None,
            team: team_name.map(str::to_owned),
            color,
            summary: wrapper_summary,
            body: nonempty(Some(raw_body)).map(str::to_owned),
            task_id: None,
            delivery_id: None,
            status: None,
            authored_at,
        },
    }
}

fn peer_name(origin: &UserOrigin) -> Option<String> {
    origin
        .name
        .as_deref()
        .or(origin.from.as_deref())
        .and_then(|value| nonempty(Some(value)))
        .map(str::to_owned)
}

fn string(value: &Option<Value>, key: &str) -> Option<String> {
    string_ref(value, key).map(str::to_owned)
}

fn string_ref<'a>(value: &'a Option<Value>, key: &str) -> Option<&'a str> {
    value
        .as_ref()
        .and_then(|value| value[key].as_str())
        .and_then(|value| nonempty(Some(value)))
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

pub(crate) fn participant_color(value: &str) -> Option<ParticipantColor> {
    match value {
        "blue" => Some(ParticipantColor::Blue),
        "green" => Some(ParticipantColor::Green),
        "yellow" => Some(ParticipantColor::Yellow),
        "purple" => Some(ParticipantColor::Purple),
        "orange" => Some(ParticipantColor::Orange),
        "pink" => Some(ParticipantColor::Pink),
        "cyan" => Some(ParticipantColor::Cyan),
        "red" => Some(ParticipantColor::Red),
        _ => None,
    }
}

fn top_level_envelopes(input: &str) -> Option<Vec<Envelope<'_>>> {
    let mut rest = input.trim();
    let mut envelopes = Vec::new();
    while !rest.is_empty() {
        let open_end = opening_tag_end(rest)?;
        let opening = &rest[1..open_end];
        let name_end = opening.find(char::is_whitespace).unwrap_or(opening.len());
        let name = &opening[..name_end];
        if !matches!(
            name,
            "agent-message" | "teammate-message" | "task-notification"
        ) {
            return None;
        }
        let attributes = parse_attributes(&opening[name_end..])?;
        let closing = format!("</{name}>");
        let body_start = open_end + 1;
        let body_end = rest[body_start..].find(&closing)? + body_start;
        envelopes.push(Envelope {
            name,
            attributes,
            body: &rest[body_start..body_end],
        });
        rest = rest[body_end + closing.len()..].trim_start();
    }
    (!envelopes.is_empty()).then_some(envelopes)
}

fn opening_tag_end(input: &str) -> Option<usize> {
    if !input.starts_with('<') || input.starts_with("</") {
        return None;
    }
    let mut quote = None;
    for (index, character) in input.char_indices().skip(1) {
        match (quote, character) {
            (Some(current), value) if value == current => quote = None,
            (None, '\'' | '"') => quote = Some(character),
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn parse_attributes(mut input: &str) -> Option<BTreeMap<String, String>> {
    let mut attributes = BTreeMap::new();
    loop {
        input = input.trim_start();
        if input.is_empty() {
            return Some(attributes);
        }
        let key_end = input
            .find(|character: char| character.is_whitespace() || character == '=')
            .unwrap_or(input.len());
        let key = &input[..key_end];
        if key.is_empty()
            || !key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "_-:".contains(character))
        {
            return None;
        }
        input = input[key_end..].trim_start();
        input = input.strip_prefix('=')?.trim_start();
        let quote = input.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        input = &input[quote.len_utf8()..];
        let value_end = input.find(quote)?;
        attributes.insert(key.to_owned(), unescape(&input[..value_end]));
        input = &input[value_end + quote.len_utf8()..];
    }
}

fn unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiple_direct_envelopes_but_not_nested_copies() {
        let text = concat!(
            "<teammate-message teammate_id=\"w1\" color=\"blue\">first</teammate-message>",
            "\n<teammate-message teammate_id=\"w2\">second</teammate-message>"
        );
        let events = classify(text, None, Some("team-a"), false).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].sender.as_deref(), Some("w1"));
        assert_eq!(events[0].color, Some(ParticipantColor::Blue));
        assert_eq!(events[1].body.as_deref(), Some("second"));

        let nested = "<task-notification><teammate-message teammate_id=\"w1\">copy</teammate-message></task-notification>";
        assert!(classify(nested, None, None, false).unwrap().is_empty());
    }

    #[test]
    fn structured_team_payloads_become_lifecycle_and_assignment_facts() {
        let assignment = r#"<teammate-message teammate_id="worker" color="green">{"type":"task_assignment","assignedBy":"lead","subject":"Fix it","description":"Keep the typed action","taskId":"7","timestamp":"2026-09-03T00:00:00Z"}</teammate-message>"#;
        let event = classify(assignment, None, Some("delta"), false)
            .unwrap()
            .remove(0);
        assert_eq!(event.kind, CollaborationEventKind::Assignment);
        assert_eq!(event.sender.as_deref(), Some("lead"));
        assert_eq!(event.recipient.as_deref(), Some("worker"));
        assert_eq!(event.task_id.as_deref(), Some("7"));
        assert_eq!(event.color, Some(ParticipantColor::Green));

        let idle = r#"<teammate-message teammate_id="worker">{"type":"idle_notification","from":"worker","idleReason":"failed","timestamp":"2026-09-03T00:00:00Z"}</teammate-message>"#;
        let event = classify(idle, None, None, false).unwrap().remove(0);
        assert_eq!(event.kind, CollaborationEventKind::Idle);
        assert_eq!(event.status, Some(CollaborationStatus::Failed));
    }

    #[test]
    fn wrapperless_peer_uses_structured_origin_body() {
        let origin = UserOrigin {
            kind: "peer".into(),
            from: Some("agent-a".into()),
            body: Some("full message".into()),
            name: Some("schema-audit".into()),
            sender_task_id: Some("task-9".into()),
            unknown: Default::default(),
        };
        let event = classify("short preview", Some(&origin), None, true)
            .unwrap()
            .remove(0);
        assert_eq!(event.sender.as_deref(), Some("schema-audit"));
        assert_eq!(event.body.as_deref(), Some("full message"));
        assert_eq!(event.task_id.as_deref(), Some("task-9"));
    }

    #[test]
    fn parses_workflow_task_notification_without_the_ephemeral_output_path() {
        let event = task_notification(concat!(
            "<task-notification>",
            "<task-id>w1</task-id><tool-use-id>toolu_1</tool-use-id>",
            "<output-file>/private/tmp/gone</output-file>",
            "<status>completed</status><summary>done</summary>",
            "<result>{\"doc\":\"/tmp/result.md\"}</result>",
            "<usage><agent_count>12</agent_count><subagent_tokens>1123053</subagent_tokens>",
            "<tool_uses>73</tool_uses><duration_ms>1890268</duration_ms></usage>",
            "</task-notification>"
        ))
        .unwrap();
        assert_eq!(event.tool_use_id.as_deref(), Some("toolu_1"));
        assert_eq!(event.status.as_deref(), Some("completed"));
        assert_eq!(event.agent_count, Some(12));
        assert_eq!(event.tokens, Some(1_123_053));
        assert_eq!(event.tool_uses, Some(73));
        assert_eq!(event.duration_ms, Some(1_890_268));
        assert_eq!(
            event.result.as_deref(),
            Some("{\"doc\":\"/tmp/result.md\"}")
        );
    }
}
