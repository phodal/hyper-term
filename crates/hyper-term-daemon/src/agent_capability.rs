use std::collections::{HashMap, HashSet};
use std::io;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hyper_term_protocol::TaskId;

use crate::DaemonError;

pub const DEFAULT_AGENT_CAPABILITY_LIFETIME: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const DEFAULT_AGENT_INVALID_REQUEST_LIMIT: usize = 8;
const MAX_AGENT_CAPABILITY_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MAX_AGENT_INVALID_REQUEST_LIMIT: usize = 64;
const MAX_AGENT_CAPABILITY_TOOLS: usize = 16;

#[derive(Clone, Debug)]
pub struct AgentCapabilityPolicy {
    pub task_id: TaskId,
    pub allowed_tools: Vec<String>,
    pub lifetime: Duration,
    pub invalid_request_limit: usize,
}

impl AgentCapabilityPolicy {
    pub fn new(
        task_id: TaskId,
        allowed_tools: impl IntoIterator<Item = String>,
    ) -> Result<Self, DaemonError> {
        let mut unique = HashSet::new();
        let mut allowed_tools = allowed_tools
            .into_iter()
            .filter(|tool| unique.insert(tool.clone()))
            .collect::<Vec<_>>();
        allowed_tools.sort_unstable();
        let policy = Self {
            task_id,
            allowed_tools,
            lifetime: DEFAULT_AGENT_CAPABILITY_LIFETIME,
            invalid_request_limit: DEFAULT_AGENT_INVALID_REQUEST_LIMIT,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn with_lifetime(mut self, lifetime: Duration) -> Result<Self, DaemonError> {
        self.lifetime = lifetime;
        self.validate()?;
        Ok(self)
    }

    pub fn with_invalid_request_limit(
        mut self,
        invalid_request_limit: usize,
    ) -> Result<Self, DaemonError> {
        self.invalid_request_limit = invalid_request_limit;
        self.validate()?;
        Ok(self)
    }

    pub(super) fn validate(&self) -> Result<(), DaemonError> {
        if self.allowed_tools.is_empty()
            || self.allowed_tools.len() > MAX_AGENT_CAPABILITY_TOOLS
            || self.lifetime.is_zero()
            || self.lifetime > MAX_AGENT_CAPABILITY_LIFETIME
            || self.invalid_request_limit == 0
            || self.invalid_request_limit > MAX_AGENT_INVALID_REQUEST_LIMIT
            || self
                .allowed_tools
                .iter()
                .any(|tool| !is_brokered_tool(tool))
        {
            return Err(DaemonError::InvalidAgentCapabilityPolicy);
        }
        Ok(())
    }
}

fn is_brokered_tool(tool: &str) -> bool {
    matches!(
        tool,
        "hyper_term.diff.review" | "hyper_term.lsp.query" | "hyper_term.genui.compile"
    )
}

#[derive(Clone, Copy, Debug)]
pub(super) enum CapabilityRevocationReason {
    ServerDropped,
    Expired,
    InvalidRequestBudget,
    ListenerFailed,
}

impl CapabilityRevocationReason {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::ServerDropped => "Agent MCP capability was revoked when the session stopped",
            Self::Expired => "Agent MCP capability expired before dispatch",
            Self::InvalidRequestBudget => {
                "Agent MCP capability exceeded its invalid-request budget"
            }
            Self::ListenerFailed => "Agent MCP capability listener stopped unexpectedly",
        }
    }
}

type RevocationCallback = Box<dyn FnOnce(CapabilityRevocationReason) + Send + 'static>;

pub(super) struct CapabilityControl {
    revoked: AtomicBool,
    expires_at: Option<Instant>,
    invalid_requests: AtomicUsize,
    invalid_request_limit: Option<usize>,
    next_connection_id: AtomicU64,
    connections: Mutex<HashMap<u64, UnixStream>>,
    on_revoke: Mutex<Option<RevocationCallback>>,
}

impl CapabilityControl {
    pub(super) fn desktop() -> Arc<Self> {
        Arc::new(Self {
            revoked: AtomicBool::new(false),
            expires_at: None,
            invalid_requests: AtomicUsize::new(0),
            invalid_request_limit: None,
            next_connection_id: AtomicU64::new(1),
            connections: Mutex::new(HashMap::new()),
            on_revoke: Mutex::new(None),
        })
    }

    pub(super) fn agent(
        policy: &AgentCapabilityPolicy,
        on_revoke: RevocationCallback,
    ) -> Result<Arc<Self>, DaemonError> {
        let expires_at = Instant::now()
            .checked_add(policy.lifetime)
            .ok_or(DaemonError::InvalidAgentCapabilityPolicy)?;
        Ok(Arc::new(Self {
            revoked: AtomicBool::new(false),
            expires_at: Some(expires_at),
            invalid_requests: AtomicUsize::new(0),
            invalid_request_limit: Some(policy.invalid_request_limit),
            next_connection_id: AtomicU64::new(1),
            connections: Mutex::new(HashMap::new()),
            on_revoke: Mutex::new(Some(on_revoke)),
        }))
    }

    pub(super) fn is_active(&self) -> bool {
        if self.revoked.load(Ordering::Acquire) {
            return false;
        }
        if self
            .expires_at
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.revoke(CapabilityRevocationReason::Expired);
            return false;
        }
        true
    }

    pub(super) fn register_connection(
        self: &Arc<Self>,
        stream: &UnixStream,
    ) -> io::Result<Option<CapabilityConnectionGuard>> {
        if !self.is_active() {
            return Ok(None);
        }
        let tracked = stream.try_clone()?;
        let connection_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);
        lock_io(&self.connections)?.insert(connection_id, tracked);
        if !self.is_active() {
            lock_io(&self.connections)?.remove(&connection_id);
            return Ok(None);
        }
        Ok(Some(CapabilityConnectionGuard {
            control: Arc::clone(self),
            connection_id,
        }))
    }

    pub(super) fn record_invalid_request(&self) -> bool {
        let Some(limit) = self.invalid_request_limit else {
            return false;
        };
        let count = self.invalid_requests.fetch_add(1, Ordering::AcqRel) + 1;
        if count >= limit {
            self.revoke(CapabilityRevocationReason::InvalidRequestBudget);
            true
        } else {
            false
        }
    }

    pub(super) fn revoke(&self, reason: CapabilityRevocationReason) {
        if self.revoked.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(mut connections) = self.connections.lock() {
            for (_, stream) in connections.drain() {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
        let callback = self
            .on_revoke
            .lock()
            .ok()
            .and_then(|mut callback| callback.take());
        if let Some(callback) = callback {
            callback(reason);
        }
    }
}

pub(super) struct CapabilityConnectionGuard {
    control: Arc<CapabilityControl>,
    connection_id: u64,
}

impl Drop for CapabilityConnectionGuard {
    fn drop(&mut self) {
        if let Ok(mut connections) = self.control.connections.lock() {
            connections.remove(&self.connection_id);
        }
    }
}

fn lock_io<T>(mutex: &Mutex<T>) -> io::Result<std::sync::MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other("capability connection registry is poisoned"))
}
