use std::io;

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
    let response = read_http_response(&mut stream)
        .await
        .unwrap_or_else(|error| panic!("read agent response for {method} {path}: {error}"));
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

async fn read_http_response(stream: &mut tokio::net::TcpStream) -> io::Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => return Ok(response),
            Ok(length) => {
                response.extend_from_slice(&chunk[..length]);
                // HTTP/1.1 frames finite Axum responses with Content-Length.
                // Stop at that boundary instead of waiting for TCP EOF: macOS
                // may report a reset when the peer closes an otherwise complete
                // Connection: close response.
                if http_response_is_complete(&response)? {
                    return Ok(response);
                }
            }
            Err(error)
                if error.kind() == io::ErrorKind::ConnectionReset
                    && http_response_is_complete(&response)? =>
            {
                return Ok(response);
            }
            Err(error) => return Err(error),
        }
    }
}

fn http_response_is_complete(response: &[u8]) -> io::Result<bool> {
    let Some(header_end) = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
    else {
        return Ok(false);
    };
    let headers = std::str::from_utf8(&response[..header_end])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 HTTP headers"))?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP status"))?;
    if (100..200).contains(&status) || matches!(status, 204 | 304) {
        return Ok(true);
    }
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim())
    });
    let Some(content_length) = content_length else {
        return Ok(false);
    };
    let content_length = content_length
        .parse::<usize>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid HTTP Content-Length"))?;
    Ok(response.len() >= header_end.saturating_add(content_length))
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

#[test]
fn http_response_completion_follows_the_declared_body_length() {
    let partial = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nok";
    assert!(!http_response_is_complete(partial).unwrap());

    let complete = b"HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\nokay";
    assert!(http_response_is_complete(complete).unwrap());
    assert!(!http_response_is_complete(b"HTTP/1.1 204 No Content\r\n").unwrap());
    assert!(http_response_is_complete(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap());
}
