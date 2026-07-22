use hyper_term_core::{OperationRecord, classify_environment_name};
use hyper_term_drivers::{McpToolClass, validate_brokered_mcp_tool};
use hyper_term_protocol::{
    APPROVAL_DETAIL_SCHEMA_VERSION, ActionDigest, ApprovalActionDetail, ApprovalDetail,
    ApprovalDetailDigest, BROKERED_MCP_TOOL_CALL_SCHEMA_VERSION, BoundApprovalDetail,
    BrokeredMcpToolCall, EnvironmentClass, McpArgumentsDigest, OperationAction, OperationId,
    OperationKind, PermissionDecision, RiskClass, TaskId, canonical_mcp_json_bytes,
};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;

use crate::DaemonError;
use crate::{
    DaemonState, validate_local_mcp_tool_call, validate_mcp_server_launch, validate_operation_scope,
};

pub(crate) fn validate_action_kind(
    kind: &OperationKind,
    action: &OperationAction,
) -> Result<(), DaemonError> {
    let valid = match (kind, action) {
        (OperationKind::Shell, OperationAction::Shell { command }) => {
            validate_operation_environment(command)?;
            true
        }
        (OperationKind::McpServerLaunch, OperationAction::McpServerLaunch { launch }) => {
            return validate_mcp_server_launch(launch);
        }
        (OperationKind::McpTool, OperationAction::McpToolCall { call }) => {
            return validate_local_mcp_tool_call(call);
        }
        (OperationKind::McpTool, OperationAction::BrokeredMcpToolCall { call }) => {
            return validate_brokered_mcp_tool_call(call);
        }
        (
            OperationKind::McpTool
            | OperationKind::FileEdit
            | OperationKind::AgentTool
            | OperationKind::ComputerUse
            | OperationKind::ArtifactBuild
            | OperationKind::Other(_),
            OperationAction::Opaque { .. },
        ) => true,
        _ => false,
    };
    if !valid {
        return Err(DaemonError::ActionKindMismatch);
    }
    Ok(())
}

fn validate_operation_environment(
    command: &hyper_term_protocol::TerminalCommand,
) -> Result<(), DaemonError> {
    for name in command.env.keys() {
        if matches!(
            classify_environment_name(name),
            EnvironmentClass::Credential
                | EnvironmentClass::ShellStartup
                | EnvironmentClass::LoaderInjection
                | EnvironmentClass::NetworkControl
                | EnvironmentClass::AuthorityHandle
        ) {
            return Err(DaemonError::UnsafeOperationEnvironment(name.clone()));
        }
    }
    Ok(())
}

impl DaemonState {
    pub fn propose_brokered_mcp_tool(
        &self,
        task_id: TaskId,
        tool_name: String,
        canonical_arguments: serde_json::Value,
    ) -> Result<OperationRecord, DaemonError> {
        let class = validate_brokered_mcp_tool(&tool_name, &canonical_arguments)
            .map_err(|_| DaemonError::BrokeredMcpBindingMismatch)?;
        let (canonical_arguments_preview, arguments_bytes, arguments_truncated) =
            bounded_arguments_preview(&canonical_arguments)?;
        let encoded_arguments = canonical_mcp_json_bytes(&canonical_arguments)
            .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?;
        let arguments_digest = McpArgumentsDigest::parse(sha256_bytes(&encoded_arguments))
            .map_err(|_| DaemonError::BrokeredMcpBindingMismatch)?;
        let proposal = serde_json::json!({
            "name": tool_name,
            "arguments": canonical_arguments,
        });
        let proposal_digest = sha256_bytes(
            &canonical_mcp_json_bytes(&proposal)
                .map_err(|error| DaemonError::BrokeredMcpRuntime(error.to_string()))?,
        );
        let (summary, required_capabilities) = match class {
            McpToolClass::GenUiCompile => (
                "Compile a bounded GenUI artifact with the supervised Deno runtime",
                vec!["artifact_build".into(), "deno_runtime".into()],
            ),
            McpToolClass::DenoLspQuery => (
                "Query the supervised Deno LSP against a workspace snapshot",
                vec!["workspace_snapshot_read".into(), "deno_lsp".into()],
            ),
            McpToolClass::DiffReview => (
                "Build a read-only diff review artifact from bounded input",
                vec!["diff_review".into()],
            ),
        };
        self.propose_operation(
            task_id,
            OperationKind::McpTool,
            OperationAction::BrokeredMcpToolCall {
                call: BrokeredMcpToolCall {
                    schema_version: BROKERED_MCP_TOOL_CALL_SCHEMA_VERSION,
                    server_id: "hyper-term".into(),
                    tool_name,
                    canonical_arguments_preview,
                    arguments_bytes,
                    arguments_truncated,
                    arguments_digest,
                    proposal_digest,
                },
            },
            summary.into(),
            RiskClass::ReadOnly,
            required_capabilities,
        )
    }

    pub fn approval_detail(
        &self,
        operation_id: OperationId,
    ) -> Result<BoundApprovalDetail, DaemonError> {
        bound_approval_detail(&self.operation(operation_id)?)
    }

    pub fn decide_permission_bound(
        &self,
        task_id: TaskId,
        operation_id: OperationId,
        expected_revision: u64,
        expected_detail_digest: &ApprovalDetailDigest,
        decision: PermissionDecision,
    ) -> Result<OperationRecord, DaemonError> {
        let record = self.operation(operation_id)?;
        validate_operation_scope(&record, task_id, expected_revision)?;
        let current = bound_approval_detail(&record)?;
        if &current.detail_digest != expected_detail_digest {
            return Err(DaemonError::ApprovalDetailMismatch);
        }
        self.decide_permission(task_id, operation_id, expected_revision, decision)
    }
}

pub(crate) fn bound_approval_detail(
    operation: &OperationRecord,
) -> Result<BoundApprovalDetail, DaemonError> {
    let action_digest = digest_value(&operation.action)?;
    let action_digest =
        ActionDigest::parse(action_digest).map_err(|_| DaemonError::InvalidApprovalDetail)?;
    let action = match &operation.action {
        OperationAction::Shell { command } => ApprovalActionDetail::Shell {
            program: command.program.clone(),
            argv: redact_command_arguments(&command.args),
            cwd: command.cwd.clone(),
            environment_keys: command.env.keys().cloned().collect(),
        },
        OperationAction::McpServerLaunch { launch } => ApprovalActionDetail::McpServerLaunch {
            server_id: launch.server_id.clone(),
            executable: launch.executable.clone(),
            executable_sha256: launch.executable_sha256.clone(),
            argument_count: launch.argument_count,
            arguments_digest: launch.arguments_digest.clone(),
            working_directory: launch.working_directory.clone(),
            sandbox_profile_digest: launch.sandbox_profile_digest.clone(),
        },
        OperationAction::McpToolCall { call } => ApprovalActionDetail::McpTool {
            server_id: call.server_id.clone(),
            tool_name: call.tool_name.clone(),
            runtime_identity_digest: call.runtime_identity_digest.clone(),
            catalog_digest: call.catalog_digest.clone(),
            tool_contract_digest: call.tool_contract_digest.clone(),
            arguments_digest: call.arguments_digest.clone(),
        },
        OperationAction::BrokeredMcpToolCall { call } => ApprovalActionDetail::BrokeredMcpTool {
            server_id: call.server_id.clone(),
            tool_name: call.tool_name.clone(),
            canonical_arguments_preview: call.canonical_arguments_preview.clone(),
            arguments_bytes: call.arguments_bytes,
            arguments_truncated: call.arguments_truncated,
            arguments_digest: call.arguments_digest.clone(),
            proposal_digest: call.proposal_digest.clone(),
        },
        OperationAction::Opaque {
            kind,
            payload_digest,
        } => ApprovalActionDetail::Opaque {
            kind: kind.clone(),
            payload_digest: payload_digest.clone(),
        },
    };
    let detail = ApprovalDetail {
        schema_version: APPROVAL_DETAIL_SCHEMA_VERSION,
        operation_id: operation.operation_id,
        operation_revision: operation.revision,
        action_digest,
        action,
        risk: operation.risk,
        effective_capabilities: operation.required_capabilities.clone(),
        opaque_effect: matches!(
            &operation.action,
            OperationAction::Opaque { kind, .. }
                if !matches!(
                    kind.as_str(),
                    "hyper_term.workspace.apply"
                        | "hyper_term.tier2.accept"
                        | "hyper_term.genui.compile"
                )
        ),
    };
    let detail_digest = ApprovalDetailDigest::parse(digest_value(&detail)?)
        .map_err(|_| DaemonError::InvalidApprovalDetail)?;
    Ok(BoundApprovalDetail {
        detail,
        detail_digest,
    })
}

pub(crate) fn validate_brokered_mcp_tool_call(
    call: &BrokeredMcpToolCall,
) -> Result<(), DaemonError> {
    if call.schema_version != hyper_term_protocol::BROKERED_MCP_TOOL_CALL_SCHEMA_VERSION
        || call.server_id != "hyper-term"
        || !matches!(
            call.tool_name.as_str(),
            "hyper_term.diff.review" | "hyper_term.lsp.query" | "hyper_term.genui.compile"
        )
        || call.canonical_arguments_preview.is_empty()
        || call.canonical_arguments_preview.len() > MAX_ARGUMENT_PREVIEW_BYTES + 4
        || call.arguments_bytes == 0
        || !is_sha256(&call.proposal_digest)
    {
        return Err(DaemonError::BrokeredMcpBindingMismatch);
    }
    Ok(())
}

const MAX_ARGUMENT_PREVIEW_BYTES: usize = 192;

fn bounded_arguments_preview(
    arguments: &serde_json::Value,
) -> Result<(String, u32, bool), DaemonError> {
    let compact = serde_json::to_vec(arguments).map_err(|_| DaemonError::InvalidApprovalDetail)?;
    let arguments_bytes =
        u32::try_from(compact.len()).map_err(|_| DaemonError::InvalidApprovalDetail)?;
    let mut preview =
        serde_json::to_string_pretty(arguments).map_err(|_| DaemonError::InvalidApprovalDetail)?;
    if preview.len() <= MAX_ARGUMENT_PREVIEW_BYTES {
        return Ok((preview, arguments_bytes, false));
    }
    let mut boundary = MAX_ARGUMENT_PREVIEW_BYTES;
    while !preview.is_char_boundary(boundary) {
        boundary -= 1;
    }
    preview.truncate(boundary);
    preview.push_str("\n…");
    Ok((preview, arguments_bytes, true))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn digest_value(value: &impl serde::Serialize) -> Result<String, DaemonError> {
    let bytes = serde_json::to_vec(value).map_err(|_| DaemonError::InvalidApprovalDetail)?;
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|_| DaemonError::InvalidApprovalDetail)?;
    }
    Ok(encoded)
}

fn redact_command_arguments(arguments: &[String]) -> Vec<String> {
    let mut redact_next = false;
    arguments
        .iter()
        .map(|argument| {
            if redact_next {
                redact_next = false;
                return "<redacted>".into();
            }
            if let Some((flag, _)) = argument.split_once('=')
                && sensitive_flag(flag)
            {
                return format!("{flag}=<redacted>");
            }
            if sensitive_flag(argument) {
                redact_next = true;
            }
            argument.clone()
        })
        .collect()
}

fn sensitive_flag(value: &str) -> bool {
    let normalized = value
        .trim_start_matches('-')
        .to_ascii_lowercase()
        .replace('_', "-");
    matches!(
        normalized.as_str(),
        "token"
            | "access-token"
            | "api-key"
            | "password"
            | "passwd"
            | "secret"
            | "authorization"
            | "credential"
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use hyper_term_core::OperationRecord;
    use hyper_term_protocol::{
        OperationAction, OperationId, OperationKind, OperationState, RiskClass, TaskId,
        TerminalCommand,
    };

    use super::*;

    #[test]
    fn shell_detail_preserves_argument_boundaries_and_redacts_credentials() {
        let operation = OperationRecord {
            task_id: TaskId::new(),
            operation_id: OperationId::new(),
            revision: 3,
            kind: OperationKind::Shell,
            action: OperationAction::Shell {
                command: TerminalCommand {
                    program: "tool".into(),
                    args: vec![
                        "--name".into(),
                        "hello world".into(),
                        "--token".into(),
                        "secret-value".into(),
                        "--api-key=another-secret".into(),
                    ],
                    cwd: Some("/workspace".into()),
                    env: BTreeMap::from([
                        ("API_TOKEN".into(), "not-rendered".into()),
                        ("LANG".into(), "en_US.UTF-8".into()),
                    ]),
                },
            },
            summary: "run tool".into(),
            risk: RiskClass::ExternalEffect,
            required_capabilities: vec!["shell".into()],
            state: OperationState::WaitingHuman,
            permission_decision: None,
        };

        let bound = bound_approval_detail(&operation).unwrap();
        assert_eq!(bound.detail.operation_revision, 3);
        assert_eq!(bound.detail_digest.as_str().len(), 64);
        let ApprovalActionDetail::Shell {
            argv,
            environment_keys,
            ..
        } = bound.detail.action
        else {
            panic!("expected shell detail");
        };
        assert_eq!(
            argv,
            [
                "--name",
                "hello world",
                "--token",
                "<redacted>",
                "--api-key=<redacted>"
            ]
        );
        assert_eq!(environment_keys, ["API_TOKEN", "LANG"]);
    }

    #[test]
    fn agent_shell_environment_requires_opaque_brokers_for_authority_classes() {
        for name in [
            "OPENAI_API_KEY",
            "BASH_ENV",
            "DYLD_INSERT_LIBRARIES",
            "HTTPS_PROXY",
            "SSH_AUTH_SOCK",
        ] {
            let action = OperationAction::Shell {
                command: TerminalCommand {
                    program: "/bin/sh".into(),
                    args: Vec::new(),
                    cwd: Some("/workspace".into()),
                    env: BTreeMap::from([(name.into(), "value".into())]),
                },
            };
            assert!(matches!(
                validate_action_kind(&OperationKind::Shell, &action),
                Err(DaemonError::UnsafeOperationEnvironment(rejected)) if rejected == name
            ));
        }

        let safe = OperationAction::Shell {
            command: TerminalCommand {
                program: "/bin/sh".into(),
                args: Vec::new(),
                cwd: Some("/workspace".into()),
                env: BTreeMap::from([("MODEL_CONFIG".into(), "value".into())]),
            },
        };
        validate_action_kind(&OperationKind::Shell, &safe).unwrap();
    }
}
