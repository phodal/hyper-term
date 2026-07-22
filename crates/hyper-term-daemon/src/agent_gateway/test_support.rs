use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

pub(super) async fn request(
    address: SocketAddr,
    token: &str,
    session_id: u16,
    method: &str,
) -> (u16, Vec<u8>) {
    request_path(
        address,
        &format!("/agent/session?token={token}&session_id={session_id}"),
        method,
        b"",
    )
    .await
}

pub(super) async fn request_path(
    address: SocketAddr,
    path: &str,
    method: &str,
    body: &[u8],
) -> (u16, Vec<u8>) {
    let (status, _, body) = request_path_raw(address, path, method, body).await;
    (status, body)
}

pub(super) async fn request_path_raw(
    address: SocketAddr,
    path: &str,
    method: &str,
    body: &[u8],
) -> (u16, Vec<u8>, Vec<u8>) {
    let mut stream = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect agent gateway");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write agent request");
    stream
        .write_all(body)
        .await
        .expect("write agent request body");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .expect("read agent response");
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .expect("HTTP response headers");
    let status = String::from_utf8_lossy(&response[..header_end])
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()
        .expect("numeric HTTP status");
    (
        status,
        response[..header_end].to_vec(),
        response[header_end..].to_vec(),
    )
}

pub(super) async fn wait_for_provider_readiness(address: SocketAddr, path: &str, expected: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let marker = format!("\"readiness\":\"{expected}\"");
    loop {
        let (status, body) = request_path(address, path, "GET", b"").await;
        if status == StatusCode::OK.as_u16() && String::from_utf8_lossy(&body).contains(&marker) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "provider readiness did not become {expected}: {}",
            String::from_utf8_lossy(&body)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
