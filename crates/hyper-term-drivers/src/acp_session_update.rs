use agent_client_protocol::schema::v1;
use hyper_term_protocol::{AgentPlanEntry, AgentPlanPriority, AgentPlanStatus};

use crate::AgentDriverEvent;
use crate::acp::{AcpAdapterError, bounded};

pub(super) fn normalize_content_update(
    sequence: u64,
    thread_id: &str,
    turn_id: &str,
    update: &v1::SessionUpdate,
) -> Result<Option<AgentDriverEvent>, AcpAdapterError> {
    let event = match update {
        v1::SessionUpdate::AgentMessageChunk(chunk) => AgentDriverEvent::MessageDelta {
            sequence,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            text: content_text(&chunk.content)?,
        },
        v1::SessionUpdate::UserMessageChunk(chunk) => AgentDriverEvent::UserMessageDelta {
            sequence,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            message_id: chunk
                .message_id
                .as_ref()
                .map(ToString::to_string)
                .map(|value| bounded(value, 4096))
                .transpose()?,
            text: content_text(&chunk.content)?,
        },
        v1::SessionUpdate::AgentThoughtChunk(chunk) => AgentDriverEvent::ThoughtDelta {
            sequence,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            text: content_text(&chunk.content)?,
        },
        v1::SessionUpdate::Plan(plan) => AgentDriverEvent::PlanUpdated {
            sequence,
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            entries: plan
                .entries
                .iter()
                .map(normalize_plan_entry)
                .collect::<Result<Vec<_>, _>>()?,
        },
        _ => return Ok(None),
    };
    Ok(Some(event))
}

fn content_text(content: &v1::ContentBlock) -> Result<String, AcpAdapterError> {
    match content {
        v1::ContentBlock::Text(text) => bounded(text.text.clone(), 64 * 1024),
        other => bounded(serde_json::to_string(other)?, 64 * 1024),
    }
}

fn normalize_plan_entry(entry: &v1::PlanEntry) -> Result<AgentPlanEntry, AcpAdapterError> {
    Ok(AgentPlanEntry {
        content: bounded(entry.content.clone(), 16 * 1024)?,
        priority: match entry.priority {
            v1::PlanEntryPriority::High => AgentPlanPriority::High,
            v1::PlanEntryPriority::Medium => AgentPlanPriority::Medium,
            v1::PlanEntryPriority::Low => AgentPlanPriority::Low,
            _ => AgentPlanPriority::Medium,
        },
        status: match entry.status {
            v1::PlanEntryStatus::Pending => AgentPlanStatus::Pending,
            v1::PlanEntryStatus::InProgress => AgentPlanStatus::InProgress,
            v1::PlanEntryStatus::Completed => AgentPlanStatus::Completed,
            _ => AgentPlanStatus::Pending,
        },
    })
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1;
    use serde_json::json;

    use super::*;

    #[test]
    fn user_chunks_preserve_message_identity_and_text() {
        let update: v1::SessionUpdate = serde_json::from_value(json!({
            "sessionUpdate": "user_message_chunk",
            "messageId": "5ee0f5a8-b508-4a0f-864d-9f69759b2087",
            "content": { "type": "text", "text": "restored prompt" }
        }))
        .unwrap();

        assert!(matches!(
            normalize_content_update(7, "session-1", "turn-1", &update).unwrap(),
            Some(AgentDriverEvent::UserMessageDelta {
                sequence: 7,
                thread_id,
                turn_id,
                message_id: Some(message_id),
                text,
            }) if thread_id == "session-1"
                && turn_id == "turn-1"
                && message_id == "5ee0f5a8-b508-4a0f-864d-9f69759b2087"
                && text == "restored prompt"
        ));
    }
}
