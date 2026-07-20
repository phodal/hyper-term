use std::time::Duration;

use hyper_term_protocol::ContextReceipt;
use thiserror::Error;

use crate::{
    AcpAdapterError, AgentDriverEvent, AgentEffectAuthorization, AgentHostResponse,
    AgentSessionCapabilities, AgentSessionConfigValue, CodexAdapterError, DriverState,
    ExternalRequestId, StructuredAgentProtocol,
};

/// Renderer-independent port used by the daemon for every structured coding agent.
/// Provider wire formats remain inside their adapters and never become journal schema.
pub trait StructuredAgentClient: Send + Sync {
    fn provider_id(&self) -> &str;
    fn protocol(&self) -> StructuredAgentProtocol;
    fn execution_context_receipts(&self) -> Vec<ContextReceipt> {
        Vec::new()
    }
    fn initialize_session(&self, timeout: Duration) -> Result<String, AgentClientError>;
    fn start_turn(
        &self,
        session_id: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, AgentClientError>;
    /// Requests cancellation of the active turn without tearing down the
    /// provider session. Delivery is acknowledged by this call; the provider's
    /// eventual completion event remains authoritative for turn state.
    fn cancel_turn(&self, session_id: &str, turn_id: &str) -> Result<(), AgentClientError>;
    fn next_event(&self, timeout: Duration) -> Result<AgentDriverEvent, AgentClientError>;
    fn session_capabilities(&self) -> Result<AgentSessionCapabilities, AgentClientError>;
    fn set_session_config_option(
        &self,
        session_id: &str,
        config_id: &str,
        value: AgentSessionConfigValue,
        timeout: Duration,
    ) -> Result<AgentSessionCapabilities, AgentClientError>;
    fn resolve_effect(
        &self,
        request_id: &ExternalRequestId,
        authorization: AgentEffectAuthorization,
    ) -> Result<(), AgentClientError>;
    fn resolve_host_request(
        &self,
        _request_id: &ExternalRequestId,
        _response: AgentHostResponse,
    ) -> Result<(), AgentClientError> {
        Err(AgentClientError::Unsupported(
            "provider does not support Agent-to-Host requests".into(),
        ))
    }
    fn state(&self) -> Result<DriverState, AgentClientError>;
    fn stderr_tail(&self) -> Result<String, AgentClientError>;
    fn close(&self) -> Result<DriverState, AgentClientError>;
}

#[derive(Debug, Error)]
pub enum AgentClientError {
    #[error(transparent)]
    Acp(#[from] AcpAdapterError),
    #[error(transparent)]
    Codex(#[from] CodexAdapterError),
    #[error("structured agent capability is unavailable: {0}")]
    Unsupported(String),
}
