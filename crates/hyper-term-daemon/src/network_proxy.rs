//! Rust-owned, loopback-only CONNECT proxy for sandboxed agent processes.
//!
//! The proxy resolves DNS outside the sandbox and connects to the selected
//! address directly. Agent processes only receive access to this listener.

use std::collections::BTreeSet;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener as StdTcpListener};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, lookup_host};
use tokio::sync::{Semaphore, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use uuid::Uuid;

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_CONNECTIONS: usize = 32;
const HEADER_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const PROXY_USERNAME: &str = "hyper-term";

#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ManagedProxyEndpoint {
    pub(crate) proxy_url: String,
    pub(crate) allowed_hosts: Vec<String>,
    credentialed_proxy_url: String,
}

impl std::fmt::Debug for ManagedProxyEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ManagedProxyEndpoint")
            .field("proxy_url", &self.proxy_url)
            .field("allowed_hosts", &self.allowed_hosts)
            .field("credentialed_proxy_url", &"<redacted>")
            .finish()
    }
}

pub(crate) struct ManagedConnectProxy {
    endpoint: ManagedProxyEndpoint,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ManagedConnectProxy {
    pub(crate) fn start(
        allowed_hosts: impl IntoIterator<Item = String>,
    ) -> Result<Self, ProxyError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| ProxyError::RuntimeUnavailable)?;
        let policy = Arc::new(ProxyPolicy::new(allowed_hosts)?);
        let listener = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let listener = TcpListener::from_std(listener)?;
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let expected_authorization = expected_authorization(&secret);
        let proxy_url = format!("http://127.0.0.1:{}", address.port());
        let credentialed_proxy_url = format!(
            "http://{PROXY_USERNAME}:{secret}@127.0.0.1:{}",
            address.port()
        );
        let allowed_hosts = policy.patterns.iter().cloned().collect();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let task = runtime.spawn(run_proxy(
            listener,
            policy,
            expected_authorization,
            shutdown_rx,
        ));

        Ok(Self {
            endpoint: ManagedProxyEndpoint {
                proxy_url,
                allowed_hosts,
                credentialed_proxy_url,
            },
            shutdown,
            task,
        })
    }

    pub(crate) fn endpoint(&self) -> &ManagedProxyEndpoint {
        &self.endpoint
    }

    pub(crate) fn credentialed_proxy_url(&self) -> &str {
        &self.endpoint.credentialed_proxy_url
    }
}

impl Drop for ManagedConnectProxy {
    fn drop(&mut self) {
        let _ = self.shutdown.send(true);
        self.task.abort();
    }
}

#[derive(Debug, Error)]
pub(crate) enum ProxyError {
    #[error("the managed network allowlist must not be empty")]
    EmptyAllowlist,
    #[error("invalid managed network host pattern: {0}")]
    InvalidHostPattern(String),
    #[error("managed proxy listener failed: {0}")]
    Io(#[from] io::Error),
    #[error("managed proxy requires an active Tokio runtime")]
    RuntimeUnavailable,
}

#[derive(Debug)]
struct ProxyPolicy {
    patterns: BTreeSet<String>,
    #[cfg(test)]
    allow_private_targets: bool,
    #[cfg(test)]
    allowed_test_ports: BTreeSet<u16>,
}

impl ProxyPolicy {
    fn new(allowed_hosts: impl IntoIterator<Item = String>) -> Result<Self, ProxyError> {
        let patterns = allowed_hosts
            .into_iter()
            .map(|pattern| normalize_pattern(&pattern))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if patterns.is_empty() {
            return Err(ProxyError::EmptyAllowlist);
        }
        Ok(Self {
            patterns,
            #[cfg(test)]
            allow_private_targets: false,
            #[cfg(test)]
            allowed_test_ports: BTreeSet::new(),
        })
    }

    fn allows(&self, host: &str) -> bool {
        let Ok(host) = normalize_host(host) else {
            return false;
        };
        self.patterns.iter().any(|pattern| {
            if let Some(suffix) = pattern.strip_prefix("*.") {
                host.len() > suffix.len()
                    && host.ends_with(suffix)
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.'
            } else {
                host == *pattern
            }
        })
    }

    fn allows_address(&self, address: IpAddr) -> bool {
        #[cfg(test)]
        if self.allow_private_targets {
            return true;
        }
        is_public_address(address)
    }

    fn allows_port(&self, port: u16) -> bool {
        if port == 443 {
            return true;
        }
        #[cfg(test)]
        return self.allowed_test_ports.contains(&port);
        #[cfg(not(test))]
        false
    }
}

async fn run_proxy(
    listener: TcpListener,
    policy: Arc<ProxyPolicy>,
    expected_authorization: String,
    mut shutdown: watch::Receiver<bool>,
) {
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            accepted = listener.accept() => {
                let Ok((stream, peer)) = accepted else {
                    break;
                };
                if !peer.ip().is_loopback() {
                    continue;
                }
                let Ok(permit) = Arc::clone(&semaphore).try_acquire_owned() else {
                    continue;
                };
                let policy = Arc::clone(&policy);
                let expected_authorization = expected_authorization.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let _ = handle_connection(stream, &policy, &expected_authorization).await;
                });
            }
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn handle_connection(
    mut inbound: TcpStream,
    policy: &ProxyPolicy,
    expected_authorization: &str,
) -> io::Result<()> {
    let request = match timeout(HEADER_TIMEOUT, read_request(&mut inbound)).await {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            let status = if error.kind() == io::ErrorKind::PermissionDenied {
                "407 Proxy Authentication Required"
            } else {
                "400 Bad Request"
            };
            return write_status(&mut inbound, status).await;
        }
        Err(_) => return write_status(&mut inbound, "408 Request Timeout").await,
    };

    if request
        .authorization
        .as_deref()
        .is_none_or(|value| !constant_time_eq(value.as_bytes(), expected_authorization.as_bytes()))
    {
        return write_status(&mut inbound, "407 Proxy Authentication Required").await;
    }
    if request.method != "CONNECT" {
        return write_status(&mut inbound, "405 Method Not Allowed").await;
    }
    let Some((host, port)) = parse_authority(&request.authority) else {
        return write_status(&mut inbound, "400 Bad Request").await;
    };
    if !policy.allows_port(port) || !policy.allows(&host) {
        return write_status(&mut inbound, "403 Forbidden").await;
    }

    let resolved = match timeout(CONNECT_TIMEOUT, lookup_host((host.as_str(), port))).await {
        Ok(Ok(addresses)) => addresses.collect::<Vec<_>>(),
        _ => return write_status(&mut inbound, "502 Bad Gateway").await,
    };
    if resolved.is_empty()
        || resolved
            .iter()
            .any(|address| !policy.allows_address(address.ip()))
    {
        return write_status(&mut inbound, "403 Forbidden").await;
    }

    let mut outbound = None;
    for address in resolved {
        if let Ok(Ok(stream)) = timeout(CONNECT_TIMEOUT, TcpStream::connect(address)).await {
            outbound = Some(stream);
            break;
        }
    }
    let Some(mut outbound) = outbound else {
        return write_status(&mut inbound, "502 Bad Gateway").await;
    };

    inbound
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    if !request.buffered.is_empty() {
        outbound.write_all(&request.buffered).await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await?;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct ConnectRequest {
    method: String,
    authority: String,
    authorization: Option<String>,
    buffered: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> io::Result<ConnectRequest> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "proxy header too large",
            ));
        }
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete proxy header",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header terminator was checked");
    let body_start = header_end + 4;
    if body_start > MAX_HEADER_BYTES || !bytes[..body_start].is_ascii() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid proxy header",
        ));
    }
    let header = std::str::from_utf8(&bytes[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid proxy header"))?;
    let mut lines = header.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut request_parts = request_line.split(' ');
    let method = request_parts.next().unwrap_or_default();
    let authority = request_parts.next().unwrap_or_default();
    let version = request_parts.next().unwrap_or_default();
    if request_parts.next().is_some()
        || method.is_empty()
        || authority.is_empty()
        || !matches!(version, "HTTP/1.0" | "HTTP/1.1")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request line",
        ));
    }
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid proxy header",
            ));
        };
        if name.eq_ignore_ascii_case("proxy-authorization") {
            if authorization.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "duplicate proxy authorization",
                ));
            }
            authorization = Some(value.trim().to_owned());
        }
    }
    Ok(ConnectRequest {
        method: method.to_owned(),
        authority: authority.to_owned(),
        authorization,
        buffered: bytes[body_start..].to_vec(),
    })
}

fn parse_authority(authority: &str) -> Option<(String, u16)> {
    let (host, port) = authority.rsplit_once(':')?;
    if host.is_empty() || host.contains(':') || host.parse::<IpAddr>().is_ok() {
        return None;
    }
    let host = normalize_host(host).ok()?;
    let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
    Some((host, port))
}

fn normalize_pattern(pattern: &str) -> Result<String, ProxyError> {
    let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
    if pattern == "*" || pattern.is_empty() {
        return Err(ProxyError::InvalidHostPattern(pattern));
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        normalize_host(suffix)
            .map(|suffix| format!("*.{suffix}"))
            .map_err(|_| ProxyError::InvalidHostPattern(pattern))
    } else {
        normalize_host(&pattern).map_err(|_| ProxyError::InvalidHostPattern(pattern))
    }
}

fn normalize_host(host: &str) -> Result<String, ()> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 || !host.is_ascii() || host.parse::<IpAddr>().is_ok() {
        return Err(());
    }
    if host.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || label.starts_with('-')
            || label.ends_with('-')
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    }) {
        return Err(());
    }
    Ok(host)
}

fn expected_authorization(secret: &str) -> String {
    let credentials = BASE64_STANDARD.encode(format!("{PROXY_USERNAME}:{secret}"));
    format!("Basic {credentials}")
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn is_public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !(first == 0
        || first == 10
        || first == 127
        || (first == 100 && (64..=127).contains(&second))
        || (first == 169 && second == 254)
        || (first == 172 && (16..=31).contains(&second))
        || (first == 192 && second == 0 && third == 0)
        || (first == 192 && second == 0 && third == 2)
        || (first == 192 && second == 168)
        || (first == 198 && (second == 18 || second == 19))
        || (first == 198 && second == 51 && third == 100)
        || (first == 203 && second == 0 && third == 113)
        || first >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    if let Some(address) = address.to_ipv4_mapped() {
        return is_public_ipv4(address);
    }
    let segments = address.segments();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] & 0xffc0) == 0xfec0
        || (segments[0] == 0x0100 && segments[1] == 0)
        || (segments[0] == 0x2001 && segments[1] == 0x0002)
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

async fn write_status(stream: &mut TcpStream, status: &str) -> io::Result<()> {
    let response = format!("HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    stream.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_is_exact_or_scoped_to_a_suffix() {
        let policy = ProxyPolicy::new(["API.OpenAI.com.".to_owned(), "*.chatgpt.com".to_owned()])
            .expect("policy");
        assert!(policy.allows("api.openai.com"));
        assert!(policy.allows("backend.chatgpt.com"));
        assert!(!policy.allows("chatgpt.com"));
        assert!(!policy.allows("notchatgpt.com"));
        assert!(!policy.allows("api.openai.com.attacker.example"));
    }

    #[test]
    fn global_wildcards_and_ip_literals_are_rejected() {
        assert!(matches!(
            ProxyPolicy::new(["*".to_owned()]),
            Err(ProxyError::InvalidHostPattern(_))
        ));
        assert!(matches!(
            ProxyPolicy::new(["127.0.0.1".to_owned()]),
            Err(ProxyError::InvalidHostPattern(_))
        ));
        assert_eq!(parse_authority("127.0.0.1:443"), None);
        assert_eq!(parse_authority("[::1]:443"), None);
    }

    #[test]
    fn starting_without_a_runtime_fails_closed() {
        assert!(matches!(
            ManagedConnectProxy::start(["example.com".to_owned()]),
            Err(ProxyError::RuntimeUnavailable)
        ));
    }

    #[test]
    fn private_reserved_and_documentation_addresses_are_not_public() {
        for address in [
            "0.0.0.0",
            "10.0.0.1",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "172.16.0.1",
            "192.168.0.1",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "2001:db8::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(
                !is_public_address(address.parse().expect("address")),
                "{address}"
            );
        }
        for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(
                is_public_address(address.parse().expect("address")),
                "{address}"
            );
        }
    }

    #[tokio::test]
    async fn proxy_requires_authentication_before_policy_or_dns() {
        let proxy = ManagedConnectProxy::start(["example.com".to_owned()]).expect("proxy");
        assert!(!proxy.endpoint.proxy_url.contains('@'));
        let secret_url = proxy.credentialed_proxy_url().to_owned();
        assert!(secret_url.contains('@'));
        assert!(!format!("{:?}", proxy.endpoint()).contains(&secret_url));
        let address = proxy.endpoint.proxy_url.rsplit_once(':').expect("port").1;
        let mut stream =
            TcpStream::connect((Ipv4Addr::LOCALHOST, address.parse::<u16>().expect("port")))
                .await
                .expect("connect");
        stream
            .write_all(b"CONNECT example.com:443 HTTP/1.1\r\n\r\n")
            .await
            .expect("request");
        let mut response = vec![0; 256];
        let count = stream.read(&mut response).await.expect("response");
        assert!(
            String::from_utf8_lossy(&response[..count])
                .contains("407 Proxy Authentication Required")
        );
    }

    #[tokio::test]
    async fn authenticated_proxy_tunnels_only_an_allowed_target() {
        let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("echo listener");
        let echo_port = echo.local_addr().expect("echo address").port();
        let mut policy = ProxyPolicy::new(["localhost".to_owned()]).expect("policy");
        policy.allow_private_targets = true;
        policy.allowed_test_ports.insert(echo_port);
        let policy = Arc::new(policy);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("proxy listener");
        let proxy_address = listener.local_addr().expect("proxy address");
        let authorization = expected_authorization("test-secret");
        let (shutdown, shutdown_rx) = watch::channel(false);
        let proxy_task = tokio::spawn(run_proxy(
            listener,
            policy,
            authorization.clone(),
            shutdown_rx,
        ));
        let echo_task = tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.expect("echo accept");
            let mut bytes = [0_u8; 4];
            stream.read_exact(&mut bytes).await.expect("echo read");
            stream.write_all(&bytes).await.expect("echo write");
        });

        let mut stream = TcpStream::connect(proxy_address)
            .await
            .expect("connect proxy");
        let request = format!(
            "CONNECT localhost:{echo_port} HTTP/1.1\r\nProxy-Authorization: {authorization}\r\n\r\nping"
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("connect request");
        let mut response = [0_u8; 39];
        stream
            .read_exact(&mut response)
            .await
            .expect("connect response");
        assert_eq!(&response, b"HTTP/1.1 200 Connection Established\r\n\r\n");
        let mut echoed = [0_u8; 4];
        stream.read_exact(&mut echoed).await.expect("tunnel read");
        assert_eq!(&echoed, b"ping");

        let _ = shutdown.send(true);
        proxy_task.await.expect("proxy task");
        echo_task.await.expect("echo task");
    }
}
