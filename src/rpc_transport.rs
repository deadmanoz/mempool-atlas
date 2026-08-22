//! Shared bounded HTTP policy for Bitcoin JSON-RPC transports.
//!
//! Membership and classification RPC both reach the configured node endpoint
//! through this seam: redirects are never followed (so the Basic credential
//! can only reach the configured origin), the request carries an explicit
//! timeout, and the response status line, headers, and body are all bounded.
//! Declared body lengths are verified and transfer-encoded framing is
//! rejected before any decoding happens.

use std::io::Read;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use thiserror::Error;

const MAX_RESPONSE_HEADER_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_STATUS_LINE_BYTES: usize = 8 * 1024;

/// Encodes a `Basic` authorization header value for the configured identity.
pub(crate) fn basic_authorization(username: &str, password: &str) -> String {
    let credentials = BASE64_STANDARD.encode(format!("{username}:{password}"));
    format!("Basic {credentials}")
}

/// Sends one JSON `POST` to the configured endpoint and returns the bounded
/// response body with its HTTP status. Any non-200 status, including a
/// redirect, is refused before the body is read.
pub(crate) fn post_json_bounded(
    url: &str,
    authorization: &str,
    timeout: Duration,
    body: Vec<u8>,
    maximum_response_bytes: usize,
) -> Result<(u16, Vec<u8>), RpcTransportError> {
    let request_started_at = Instant::now();
    let response = minreq::post(url)
        .with_timeout(timeout.as_secs().max(1))
        .with_follow_redirects(false)
        .with_max_headers_size(MAX_RESPONSE_HEADER_BYTES)
        .with_max_status_line_length(MAX_RESPONSE_STATUS_LINE_BYTES)
        .with_header("Authorization", authorization)
        .with_header("Accept", "application/json")
        .with_header("Content-Type", "application/json")
        .with_body(body)
        .send_lazy()
        .map_err(RpcTransportError::Transport)?;
    if request_started_at.elapsed() >= timeout {
        return Err(RpcTransportError::DeadlineExceeded);
    }
    if response.status_code != 200 {
        return Err(RpcTransportError::UnexpectedHttpStatus(
            response.status_code,
        ));
    }
    read_bounded_response(
        response,
        maximum_response_bytes,
        request_started_at,
        timeout,
    )
}

fn read_bounded_response(
    mut response: minreq::ResponseLazy,
    maximum: usize,
    request_started_at: Instant,
    timeout: Duration,
) -> Result<(u16, Vec<u8>), RpcTransportError> {
    let status_code = response.status_code;
    if response
        .headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))
    {
        return Err(RpcTransportError::UnsupportedTransferEncoding);
    }
    let announced_length = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.as_str())
        .and_then(|length| length.trim().parse::<usize>().ok());
    if let Some(actual) = announced_length
        && actual > maximum
    {
        return Err(RpcTransportError::ResponseTooLarge { actual, maximum });
    }
    let mut body = Vec::with_capacity(announced_length.unwrap_or(0).min(maximum));
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if request_started_at.elapsed() >= timeout {
            return Err(RpcTransportError::DeadlineExceeded);
        }
        let remaining = maximum.saturating_sub(body.len());
        let read_limit = remaining.saturating_add(1).min(buffer.len());
        let read = response
            .read(&mut buffer[..read_limit])
            .map_err(|source| RpcTransportError::Transport(source.into()))?;
        if read == 0 {
            break;
        }
        if request_started_at.elapsed() >= timeout {
            return Err(RpcTransportError::DeadlineExceeded);
        }
        if read > remaining {
            return Err(RpcTransportError::ResponseTooLarge {
                actual: maximum.saturating_add(1),
                maximum,
            });
        }
        body.extend_from_slice(&buffer[..read]);
    }
    if let Some(expected) = announced_length
        && body.len() != expected
    {
        return Err(RpcTransportError::IncompleteResponseBody {
            expected,
            actual: body.len(),
        });
    }
    Ok((status_code, body))
}

#[derive(Debug, Error)]
pub enum RpcTransportError {
    #[error("RPC HTTP transport failed: {0}")]
    Transport(#[source] minreq::Error),
    #[error("RPC request exceeded its configured deadline")]
    DeadlineExceeded,
    #[error("RPC endpoint returned unexpected HTTP status {0}")]
    UnexpectedHttpStatus(u16),
    #[error("RPC response used unsupported Transfer-Encoding")]
    UnsupportedTransferEncoding,
    #[error("RPC response ended after {actual} of {expected} declared bytes")]
    IncompleteResponseBody { expected: usize, actual: usize },
    #[error("RPC response used {actual} bytes, exceeding the {maximum}-byte limit")]
    ResponseTooLarge { actual: usize, maximum: usize },
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::thread;

    use super::*;

    #[test]
    fn subsecond_deadline_is_not_rounded_up_for_the_socket_timeout() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("connection");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).expect("read request");

            thread::sleep(Duration::from_millis(10));
            let body = b"{}";
            if stream
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .is_err()
            {
                return;
            }
            let _ = stream.write_all(body);
        });

        let result = post_json_bounded(
            &format!("http://{address}/"),
            "Basic YXRsYXM6c2VjcmV0",
            Duration::from_millis(1),
            br#"{}"#.to_vec(),
            4096,
        );
        server.join().expect("fixture server");

        assert!(matches!(result, Err(RpcTransportError::DeadlineExceeded)));
    }
}
