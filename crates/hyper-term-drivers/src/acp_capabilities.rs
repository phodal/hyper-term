//! Bounded ACP session controls projected into the provider-neutral composer.

use std::collections::HashSet;

use agent_client_protocol::schema::v1;

use crate::acp::{AcpAdapterError, bounded};
use crate::{
    AgentAvailableCommand, AgentSessionCapabilities, AgentSessionConfigChoice,
    AgentSessionConfigKind, AgentSessionConfigOption, AgentSessionConfigValue,
};

pub(super) const ACP_SESSION_MODE_CONFIG_ID: &str = "acp.session_mode";
pub(super) const MAX_AVAILABLE_COMMANDS: usize = 96;
const MAX_SESSION_CONFIG_OPTIONS: usize = 24;
const MAX_SESSION_CONFIG_CHOICES: usize = 96;
pub(super) const MAX_CAPABILITY_ID_BYTES: usize = 128;
const MAX_CAPABILITY_LABEL_BYTES: usize = 256;
const MAX_CAPABILITY_DESCRIPTION_BYTES: usize = 2048;

pub(super) fn normalize_session_capabilities(
    modes: Option<v1::SessionModeState>,
    options: Vec<v1::SessionConfigOption>,
) -> Result<Vec<AgentSessionConfigOption>, AcpAdapterError> {
    let mut normalized = normalize_config_options(options)?;
    if let Some(mode) = modes.map(normalize_session_modes).transpose()? {
        if normalized.len() == MAX_SESSION_CONFIG_OPTIONS {
            return Err(AcpAdapterError::InvalidMessage(
                "ACP session controls exceeded their option bound".into(),
            ));
        }
        normalized.insert(0, mode);
    }
    Ok(normalized)
}

pub(super) fn replace_config_options_preserving_mode(
    capabilities: &mut AgentSessionCapabilities,
    options: Vec<v1::SessionConfigOption>,
) -> Result<(), AcpAdapterError> {
    let mode = capabilities
        .config_options
        .iter()
        .find(|option| option.id == ACP_SESSION_MODE_CONFIG_ID)
        .cloned();
    let mut normalized = normalize_config_options(options)?;
    if let Some(mode) = mode {
        if normalized.len() == MAX_SESSION_CONFIG_OPTIONS {
            return Err(AcpAdapterError::InvalidMessage(
                "ACP session controls exceeded their option bound".into(),
            ));
        }
        normalized.insert(0, mode);
    }
    capabilities.config_options = normalized;
    Ok(())
}

pub(super) fn update_session_mode(
    capabilities: &mut AgentSessionCapabilities,
    current_mode_id: String,
) -> Result<(), AcpAdapterError> {
    let current_mode_id = bounded(current_mode_id, MAX_CAPABILITY_ID_BYTES)?;
    let mode = capabilities
        .config_options
        .iter_mut()
        .find(|option| option.id == ACP_SESSION_MODE_CONFIG_ID)
        .ok_or_else(|| {
            AcpAdapterError::InvalidMessage(
                "ACP current_mode_update arrived without advertised modes".into(),
            )
        })?;
    if !mode
        .choices
        .iter()
        .any(|choice| choice.value == current_mode_id)
    {
        return Err(AcpAdapterError::InvalidMessage(format!(
            "ACP selected unavailable session mode {current_mode_id}"
        )));
    }
    mode.kind = AgentSessionConfigKind::Select {
        current_value: current_mode_id,
    };
    Ok(())
}

fn normalize_session_modes(
    modes: v1::SessionModeState,
) -> Result<AgentSessionConfigOption, AcpAdapterError> {
    if modes.available_modes.is_empty() || modes.available_modes.len() > MAX_SESSION_CONFIG_CHOICES
    {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP session modes exceeded their choice bound".into(),
        ));
    }
    let current_value = bounded(modes.current_mode_id.to_string(), MAX_CAPABILITY_ID_BYTES)?;
    let mut seen = HashSet::with_capacity(modes.available_modes.len());
    let mut choices = Vec::with_capacity(modes.available_modes.len());
    for mode in modes.available_modes {
        let value = bounded(mode.id.to_string(), MAX_CAPABILITY_ID_BYTES)?;
        if !seen.insert(value.clone()) {
            return Err(AcpAdapterError::InvalidMessage(format!(
                "ACP repeated session mode {value}"
            )));
        }
        choices.push(AgentSessionConfigChoice {
            value,
            name: bounded(mode.name, MAX_CAPABILITY_LABEL_BYTES)?,
            description: mode
                .description
                .map(|value| bounded(value, MAX_CAPABILITY_DESCRIPTION_BYTES))
                .transpose()?,
            group: None,
        });
    }
    if !choices.iter().any(|choice| choice.value == current_value) {
        return Err(AcpAdapterError::InvalidMessage(format!(
            "ACP current session mode {current_value} is unavailable"
        )));
    }
    Ok(AgentSessionConfigOption {
        id: ACP_SESSION_MODE_CONFIG_ID.into(),
        name: "Mode".into(),
        description: Some("Agent behavior and tool access for the next turn".into()),
        category: Some("mode".into()),
        kind: AgentSessionConfigKind::Select { current_value },
        choices,
    })
}

pub(super) fn normalize_config_options(
    options: Vec<v1::SessionConfigOption>,
) -> Result<Vec<AgentSessionConfigOption>, AcpAdapterError> {
    if options.len() > MAX_SESSION_CONFIG_OPTIONS {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP session configuration exceeded its option bound".into(),
        ));
    }
    options.into_iter().map(normalize_config_option).collect()
}

fn normalize_config_option(
    option: v1::SessionConfigOption,
) -> Result<AgentSessionConfigOption, AcpAdapterError> {
    let id = bounded(option.id.to_string(), MAX_CAPABILITY_ID_BYTES)?;
    if id == ACP_SESSION_MODE_CONFIG_ID {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP session configuration used Hyper Term's reserved mode ID".into(),
        ));
    }
    let name = bounded(option.name, MAX_CAPABILITY_LABEL_BYTES)?;
    let description = option
        .description
        .map(|value| bounded(value, MAX_CAPABILITY_DESCRIPTION_BYTES))
        .transpose()?;
    let category = option.category.and_then(|category| match category {
        v1::SessionConfigOptionCategory::Mode => Some("mode".to_owned()),
        v1::SessionConfigOptionCategory::Model => Some("model".to_owned()),
        v1::SessionConfigOptionCategory::ModelConfig => Some("model_config".to_owned()),
        v1::SessionConfigOptionCategory::ThoughtLevel => Some("thought_level".to_owned()),
        v1::SessionConfigOptionCategory::Other(value) => {
            bounded(value, MAX_CAPABILITY_ID_BYTES).ok()
        }
        _ => None,
    });
    let (kind, choices) = match option.kind {
        v1::SessionConfigKind::Select(select) => {
            let current_value = bounded(select.current_value.to_string(), MAX_CAPABILITY_ID_BYTES)?;
            let choices = normalize_config_choices(select.options)?;
            if !choices.iter().any(|choice| choice.value == current_value) {
                return Err(AcpAdapterError::InvalidMessage(format!(
                    "ACP session configuration {id} selected an unavailable value"
                )));
            }
            (AgentSessionConfigKind::Select { current_value }, choices)
        }
        v1::SessionConfigKind::Boolean(boolean) => (
            AgentSessionConfigKind::Boolean {
                current_value: boolean.current_value,
            },
            Vec::new(),
        ),
        _ => {
            return Err(AcpAdapterError::InvalidMessage(format!(
                "ACP session configuration {id} has an unsupported kind"
            )));
        }
    };
    Ok(AgentSessionConfigOption {
        id,
        name,
        description,
        category,
        kind,
        choices,
    })
}

fn normalize_config_choices(
    options: v1::SessionConfigSelectOptions,
) -> Result<Vec<AgentSessionConfigChoice>, AcpAdapterError> {
    let mut choices = Vec::new();
    match options {
        v1::SessionConfigSelectOptions::Ungrouped(options) => {
            for option in options {
                choices.push(normalize_config_choice(option, None)?);
            }
        }
        v1::SessionConfigSelectOptions::Grouped(groups) => {
            for group in groups {
                let group_name = bounded(group.name, MAX_CAPABILITY_LABEL_BYTES)?;
                for option in group.options {
                    choices.push(normalize_config_choice(option, Some(group_name.clone()))?);
                }
            }
        }
        _ => {
            return Err(AcpAdapterError::InvalidMessage(
                "ACP session configuration uses unsupported choice grouping".into(),
            ));
        }
    }
    if choices.len() > MAX_SESSION_CONFIG_CHOICES {
        return Err(AcpAdapterError::InvalidMessage(
            "ACP session configuration exceeded its choice bound".into(),
        ));
    }
    Ok(choices)
}

fn normalize_config_choice(
    option: v1::SessionConfigSelectOption,
    group: Option<String>,
) -> Result<AgentSessionConfigChoice, AcpAdapterError> {
    Ok(AgentSessionConfigChoice {
        value: bounded(option.value.to_string(), MAX_CAPABILITY_ID_BYTES)?,
        name: bounded(option.name, MAX_CAPABILITY_LABEL_BYTES)?,
        description: option
            .description
            .map(|value| bounded(value, MAX_CAPABILITY_DESCRIPTION_BYTES))
            .transpose()?,
        group,
    })
}

pub(super) struct NormalizedAvailableCommands {
    pub(super) commands: Vec<AgentAvailableCommand>,
    pub(super) truncated: bool,
}

pub(super) fn normalize_available_commands(
    mut commands: Vec<v1::AvailableCommand>,
) -> NormalizedAvailableCommands {
    let received = commands.len();
    // Catalogs are optional composer metadata. Prioritize Skills, preserve
    // provider order within each group, and degrade oversized input instead of
    // aborting the active turn.
    commands.sort_by_key(|command| available_command_priority(&command.name));

    let mut normalized = Vec::with_capacity(received.min(MAX_AVAILABLE_COMMANDS));
    let mut seen = HashSet::with_capacity(received.min(MAX_AVAILABLE_COMMANDS));
    let mut truncated = false;
    for command in commands {
        if normalized.len() == MAX_AVAILABLE_COMMANDS {
            truncated = true;
            break;
        }
        let command = match normalize_available_command(command) {
            Ok(command) => command,
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        if !seen.insert(command.name.clone()) {
            truncated = true;
            continue;
        }
        normalized.push(command);
    }
    truncated |= normalized.len() < received;
    NormalizedAvailableCommands {
        commands: normalized,
        truncated,
    }
}

fn available_command_priority(name: &str) -> u8 {
    if name == "skills" {
        0
    } else if name.starts_with('$') {
        1
    } else {
        2
    }
}

fn normalize_available_command(
    command: v1::AvailableCommand,
) -> Result<AgentAvailableCommand, AcpAdapterError> {
    let input_hint = match command.input {
        Some(v1::AvailableCommandInput::Unstructured(input)) => {
            Some(bounded(input.hint, MAX_CAPABILITY_DESCRIPTION_BYTES)?)
        }
        _ => None,
    };
    Ok(AgentAvailableCommand {
        name: bounded(command.name, MAX_CAPABILITY_ID_BYTES)?,
        description: bounded(command.description, MAX_CAPABILITY_DESCRIPTION_BYTES)?,
        input_hint,
    })
}

pub(super) fn validate_config_value(
    capabilities: &AgentSessionCapabilities,
    config_id: &str,
    value: &AgentSessionConfigValue,
) -> Result<(), AcpAdapterError> {
    let option = capabilities
        .config_options
        .iter()
        .find(|option| option.id == config_id)
        .ok_or_else(|| {
            AcpAdapterError::InvalidMessage(format!(
                "ACP session configuration {config_id} is unavailable"
            ))
        })?;
    match (&option.kind, value) {
        (AgentSessionConfigKind::Select { .. }, AgentSessionConfigValue::Id { value })
            if option.choices.iter().any(|choice| choice.value == *value) =>
        {
            Ok(())
        }
        (AgentSessionConfigKind::Boolean { .. }, AgentSessionConfigValue::Boolean { .. }) => Ok(()),
        _ => Err(AcpAdapterError::InvalidMessage(format!(
            "ACP session configuration {config_id} rejected the requested value"
        ))),
    }
}
