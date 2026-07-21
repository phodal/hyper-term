use std::time::Duration;

const DEFAULT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(10);
const SLOW_ACP_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn timeout(provider_id: &str) -> Duration {
    match provider_id {
        "claude-acp" | "copilot-acp" => SLOW_ACP_INITIALIZE_TIMEOUT,
        _ => DEFAULT_INITIALIZE_TIMEOUT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_codex_and_codex_acp_keep_the_fast_startup_budget() {
        assert_eq!(timeout("codex"), DEFAULT_INITIALIZE_TIMEOUT);
        assert_eq!(timeout("codex-acp"), DEFAULT_INITIALIZE_TIMEOUT);
    }

    #[test]
    fn slow_acp_adapters_can_refresh_credentials_during_startup() {
        assert_eq!(timeout("claude-acp"), SLOW_ACP_INITIALIZE_TIMEOUT);
        assert_eq!(timeout("copilot-acp"), SLOW_ACP_INITIALIZE_TIMEOUT);
    }
}
