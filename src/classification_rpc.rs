//! Wire-preserving, bounded JSON-RPC batches for classification facts.
//!
//! Bitcoin Core's membership RPC remains owned by `rpc`. This module exists
//! because classification needs the exact distinction between a present
//! JSON `null` result and an omitted `result` member. Response bodies are
//! bounded while reading; declared lengths are verified, close-delimited
//! bodies are accepted, and transfer-encoded framing is rejected.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use thiserror::Error;

use crate::rpc_transport::{self, RpcTransportError};

const JSON_RPC_VERSION: &str = "2.0";

pub(crate) struct ClassificationRpcClient {
    url: String,
    authorization: String,
    timeout: Duration,
    maximum_response_bytes: usize,
    next_id: AtomicU64,
}

impl ClassificationRpcClient {
    pub(crate) fn new(
        url: &str,
        username: String,
        password: String,
        timeout: Duration,
        maximum_response_bytes: usize,
    ) -> Self {
        Self {
            url: url.to_owned(),
            authorization: rpc_transport::basic_authorization(&username, &password),
            timeout,
            maximum_response_bytes,
            next_id: AtomicU64::new(1),
        }
    }

    pub(crate) fn send_batch(
        &self,
        method: &str,
        parameters: &[Value],
    ) -> Result<ClassificationRpcBatch, ClassificationRpcError> {
        let ids = self.reserve_ids(parameters.len())?;
        let requests = parameters
            .iter()
            .zip(ids.iter().copied())
            .map(|(params, id)| WireRequest {
                jsonrpc: JSON_RPC_VERSION,
                method,
                params,
                id,
            })
            .collect::<Vec<_>>();
        let body = serde_json::to_vec(&requests).map_err(ClassificationRpcError::EncodeRequest)?;
        let (status_code, body) = rpc_transport::post_json_bounded(
            &self.url,
            &self.authorization,
            self.timeout,
            body,
            self.maximum_response_bytes,
        )?;
        let response_bytes = body.len();
        let responses = decode_batch_response(&body, status_code, &ids)?;
        Ok(ClassificationRpcBatch {
            responses,
            response_bytes,
        })
    }

    fn reserve_ids(&self, count: usize) -> Result<Vec<u64>, ClassificationRpcError> {
        if count == 0 {
            return Err(ClassificationRpcError::EmptyBatch);
        }
        let count = u64::try_from(count).map_err(|_| ClassificationRpcError::RequestIdExhausted)?;
        let start = self
            .next_id
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(count)
            })
            .map_err(|_| ClassificationRpcError::RequestIdExhausted)?;
        Ok((start..start + count).collect())
    }
}

impl fmt::Debug for ClassificationRpcClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClassificationRpcClient")
            .field("timeout", &self.timeout)
            .field("maximum_response_bytes", &self.maximum_response_bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(crate) struct ClassificationRpcBatch {
    pub(crate) responses: Vec<Option<ClassificationRpcResponse>>,
    pub(crate) response_bytes: usize,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ClassificationRpcResponse {
    #[serde(default, deserialize_with = "deserialize_result_member")]
    result: ResultMember,
    #[serde(default, deserialize_with = "deserialize_error_member")]
    error: ErrorMember,
    id: u64,
    #[serde(default)]
    jsonrpc: Option<String>,
}

impl ClassificationRpcResponse {
    pub(crate) fn outcome(&self) -> ClassificationRpcOutcome<'_> {
        if self.jsonrpc.as_deref() != Some(JSON_RPC_VERSION) {
            return ClassificationRpcOutcome::ProtocolFailure;
        }
        match &self.error {
            ErrorMember::Value(error) => serde_json::from_str::<RpcErrorObject>(error.get())
                .map(|error| ClassificationRpcOutcome::RpcError(error.code))
                .unwrap_or(ClassificationRpcOutcome::Malformed),
            ErrorMember::Null => ClassificationRpcOutcome::Malformed,
            ErrorMember::Missing => match &self.result {
                ResultMember::Value(result) => ClassificationRpcOutcome::Value(result),
                ResultMember::Null => ClassificationRpcOutcome::Null,
                ResultMember::Missing => ClassificationRpcOutcome::Malformed,
            },
        }
    }
}

#[derive(Debug)]
pub(crate) enum ClassificationRpcOutcome<'a> {
    Value(&'a RawValue),
    Null,
    RpcError(i32),
    ProtocolFailure,
    Malformed,
}

#[derive(Debug, Default)]
enum ResultMember {
    #[default]
    Missing,
    Null,
    Value(Box<RawValue>),
}

fn deserialize_result_member<'de, D>(deserializer: D) -> Result<ResultMember, D::Error>
where
    D: Deserializer<'de>,
{
    let result = Box::<RawValue>::deserialize(deserializer)?;
    if result.get().trim() == "null" {
        Ok(ResultMember::Null)
    } else {
        Ok(ResultMember::Value(result))
    }
}

#[derive(Debug, Default)]
enum ErrorMember {
    #[default]
    Missing,
    Null,
    Value(Box<RawValue>),
}

fn deserialize_error_member<'de, D>(deserializer: D) -> Result<ErrorMember, D::Error>
where
    D: Deserializer<'de>,
{
    let error = Box::<RawValue>::deserialize(deserializer)?;
    if error.get().trim() == "null" {
        Ok(ErrorMember::Null)
    } else {
        Ok(ErrorMember::Value(error))
    }
}

#[derive(Debug, Deserialize)]
struct RpcErrorObject {
    code: i32,
    #[serde(rename = "message")]
    _message: String,
    #[serde(default, rename = "data")]
    _data: Option<Box<RawValue>>,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    params: &'a Value,
    id: u64,
}

fn decode_batch_response(
    body: &[u8],
    status_code: u16,
    expected_ids: &[u64],
) -> Result<Vec<Option<ClassificationRpcResponse>>, ClassificationRpcError> {
    let responses =
        serde_json::from_slice::<Vec<ClassificationRpcResponse>>(body).map_err(|source| {
            ClassificationRpcError::DecodeResponse {
                status_code,
                source,
            }
        })?;
    if responses.len() > expected_ids.len() {
        return Err(ClassificationRpcError::WrongBatchResponseSize);
    }
    let mut by_id = HashMap::with_capacity(responses.len());
    for response in responses {
        let id = response.id;
        if by_id.insert(id, response).is_some() {
            return Err(ClassificationRpcError::DuplicateResponseId(id));
        }
    }
    let ordered = expected_ids
        .iter()
        .map(|id| by_id.remove(id))
        .collect::<Vec<_>>();
    if let Some(id) = by_id.keys().next().copied() {
        return Err(ClassificationRpcError::UnexpectedResponseId(id));
    }
    Ok(ordered)
}

impl From<RpcTransportError> for ClassificationRpcError {
    fn from(error: RpcTransportError) -> Self {
        match error {
            RpcTransportError::Transport(source) => Self::Transport(source),
            RpcTransportError::DeadlineExceeded => Self::DeadlineExceeded,
            RpcTransportError::UnexpectedHttpStatus(status) => Self::UnexpectedHttpStatus(status),
            RpcTransportError::UnsupportedTransferEncoding => Self::UnsupportedTransferEncoding,
            RpcTransportError::IncompleteResponseBody { expected, actual } => {
                Self::IncompleteResponseBody { expected, actual }
            }
            RpcTransportError::ResponseTooLarge { actual, maximum } => {
                Self::ResponseTooLarge { actual, maximum }
            }
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ClassificationRpcError {
    #[error("classification RPC batches cannot be empty")]
    EmptyBatch,
    #[error("classification RPC request IDs are exhausted")]
    RequestIdExhausted,
    #[error("failed to encode classification RPC request: {0}")]
    EncodeRequest(#[source] serde_json::Error),
    #[error("classification RPC HTTP transport failed: {0}")]
    Transport(#[source] minreq::Error),
    #[error("classification RPC request exceeded its configured deadline")]
    DeadlineExceeded,
    #[error("classification RPC returned unexpected HTTP status {0}")]
    UnexpectedHttpStatus(u16),
    #[error("classification RPC response used unsupported Transfer-Encoding")]
    UnsupportedTransferEncoding,
    #[error("classification RPC response ended after {actual} of {expected} declared bytes")]
    IncompleteResponseBody { expected: usize, actual: usize },
    #[error("classification RPC HTTP {status_code} response was not a valid JSON-RPC batch")]
    DecodeResponse {
        status_code: u16,
        #[source]
        source: serde_json::Error,
    },
    #[error("classification RPC response used {actual} bytes, exceeding the {maximum}-byte limit")]
    ResponseTooLarge { actual: usize, maximum: usize },
    #[error("classification RPC batch returned more responses than requests")]
    WrongBatchResponseSize,
    #[error("classification RPC batch returned duplicate response ID {0}")]
    DuplicateResponseId(u64),
    #[error("classification RPC batch returned unexpected response ID {0}")]
    UnexpectedResponseId(u64),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::{self, JoinHandle};

    use serde_json::json;

    use super::*;

    #[test]
    fn result_member_preserves_value_null_and_omission() {
        let body = br#"[
            {"jsonrpc":"2.0","result":null,"id":1},
            {"jsonrpc":"2.0","id":2},
            {"jsonrpc":"2.0","result":{"ok":true},"id":3},
            {"jsonrpc":"2.0","result":{"ok":true},"error":null,"id":4}
        ]"#;

        let responses = decode_batch_response(body, 200, &[1, 2, 3, 4]).expect("valid batch");

        assert!(matches!(
            responses[0]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::Null)
        ));
        assert!(matches!(
            responses[1]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::Malformed)
        ));
        assert_eq!(
            match responses[2].as_ref().expect("value result").outcome() {
                ClassificationRpcOutcome::Value(result) => result.get(),
                outcome => panic!("unexpected outcome: {outcome:?}"),
            },
            r#"{"ok":true}"#
        );
        assert!(matches!(
            responses[3]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::Malformed)
        ));
    }

    #[test]
    fn response_outcomes_preserve_error_precedence_and_protocol_failures() {
        let body = br#"[
            {"jsonrpc":"2.0","result":{"ok":true},"error":{"code":-5,"message":"gone"},"id":1},
            {"jsonrpc":"2.0","error":{"code":-32601,"message":"missing"},"id":2},
            {"jsonrpc":"1.0","result":{"ok":true},"id":3},
            {"result":{"ok":true},"id":4},
            {"jsonrpc":"2.0","result":null,"error":{"code":-5,"message":"gone"},"id":5},
            {"jsonrpc":"2.0","error":{"message":"no code"},"id":6}
        ]"#;
        let responses =
            decode_batch_response(body, 200, &[1, 2, 3, 4, 5, 6]).expect("valid envelopes");

        assert!(matches!(
            responses[0]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::RpcError(-5))
        ));
        assert!(matches!(
            responses[1]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::RpcError(-32601))
        ));
        assert!(matches!(
            responses[2]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::ProtocolFailure)
        ));
        assert!(matches!(
            responses[3]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::ProtocolFailure)
        ));
        assert!(matches!(
            responses[4]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::RpcError(-5))
        ));
        assert!(matches!(
            responses[5]
                .as_ref()
                .map(ClassificationRpcResponse::outcome),
            Some(ClassificationRpcOutcome::Malformed)
        ));
    }

    #[test]
    fn response_ids_are_reordered_and_missing_ids_remain_empty() {
        let body = br#"[
            {"jsonrpc":"2.0","result":3,"id":3},
            {"jsonrpc":"2.0","result":1,"id":1}
        ]"#;

        let responses = decode_batch_response(body, 200, &[1, 2, 3]).expect("reordered batch");

        assert_eq!(responses.len(), 3);
        assert!(responses[0].is_some());
        assert!(responses[1].is_none());
        assert!(responses[2].is_some());
    }

    #[test]
    fn duplicate_unexpected_and_excess_response_ids_are_rejected() {
        let duplicate = br#"[
            {"jsonrpc":"2.0","result":1,"id":1},
            {"jsonrpc":"2.0","result":2,"id":1}
        ]"#;
        assert!(matches!(
            decode_batch_response(duplicate, 200, &[1, 2]),
            Err(ClassificationRpcError::DuplicateResponseId(1))
        ));

        let unexpected = br#"[{"jsonrpc":"2.0","result":1,"id":9}]"#;
        assert!(matches!(
            decode_batch_response(unexpected, 200, &[1, 2]),
            Err(ClassificationRpcError::UnexpectedResponseId(9))
        ));

        let excess = br#"[
            {"jsonrpc":"2.0","result":1,"id":1},
            {"jsonrpc":"2.0","result":2,"id":2}
        ]"#;
        assert!(matches!(
            decode_batch_response(excess, 200, &[1]),
            Err(ClassificationRpcError::WrongBatchResponseSize)
        ));
    }

    #[test]
    fn malformed_and_non_array_responses_are_rejected_without_body_echo() {
        for body in [
            b"not JSON".as_slice(),
            br#"{"jsonrpc":"2.0","id":1}"#,
            br#"[1]"#,
            br#"[{"jsonrpc":"2.0","result":1}]"#,
            br#"[{"jsonrpc":"2.0","result":1,"id":"1"}]"#,
            b"[\xff]".as_slice(),
        ] {
            let error = decode_batch_response(body, 503, &[1]).expect_err("invalid batch");
            assert!(matches!(
                error,
                ClassificationRpcError::DecodeResponse {
                    status_code: 503,
                    ..
                }
            ));
            assert!(!error.to_string().contains("not JSON"));
        }
    }

    #[test]
    fn empty_batch_is_rejected_before_dispatch() {
        let client = ClassificationRpcClient::new(
            "http://127.0.0.1:1/",
            "atlas".to_owned(),
            "secret".to_owned(),
            Duration::from_secs(1),
            1024,
        );

        assert!(matches!(
            client.send_batch("gettxout", &[]),
            Err(ClassificationRpcError::EmptyBatch)
        ));
    }

    #[test]
    fn concurrent_id_reservations_are_disjoint() {
        let client = Arc::new(ClassificationRpcClient::new(
            "http://127.0.0.1:1/",
            "atlas".to_owned(),
            "secret".to_owned(),
            Duration::from_secs(1),
            1024,
        ));
        let reservations = (0..8)
            .map(|_| {
                let client = Arc::clone(&client);
                thread::spawn(move || client.reserve_ids(32).expect("reserved IDs"))
            })
            .map(|thread| thread.join().expect("reservation thread"))
            .collect::<Vec<_>>();
        let ids = reservations.into_iter().flatten().collect::<BTreeSet<_>>();

        assert_eq!(ids.len(), 256);
        assert_eq!(ids.first(), Some(&1));
        assert_eq!(ids.last(), Some(&256));
    }

    #[test]
    fn client_debug_output_redacts_credentials() {
        let client = ClassificationRpcClient::new(
            "http://url-user:url-password@127.0.0.1:1/",
            "private-user".to_owned(),
            "private-password".to_owned(),
            Duration::from_secs(1),
            1024,
        );
        let debug = format!("{client:?}");

        assert!(!debug.contains("private-user"));
        assert!(!debug.contains("private-password"));
        assert!(!debug.contains("url-user"));
        assert!(!debug.contains("url-password"));
        assert!(!debug.contains("Basic"));
    }

    #[test]
    fn http_client_sends_basic_auth_and_accepts_exact_body_limit() {
        let response = br#"[{"jsonrpc":"2.0","result":{"ok":true},"id":1}]"#;
        let (address, request, server) = serve_once(response, Duration::ZERO);
        let client = ClassificationRpcClient::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            Duration::from_secs(2),
            response.len(),
        );

        let batch = client
            .send_batch("gettxout", &[json!(["txid", 0, false])])
            .expect("batch response");
        let request = String::from_utf8(request.recv().expect("captured request"))
            .expect("HTTP request text");
        server.join().expect("fixture server");

        assert_eq!(batch.responses.len(), 1);
        assert_eq!(batch.response_bytes, response.len());
        let header_end = request
            .as_bytes()
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("request header end");
        assert!(
            request[..header_end]
                .to_ascii_lowercase()
                .contains("authorization: basic yxrsyxm6c2vjcmv0")
        );
        let requests = serde_json::from_str::<Value>(&request[header_end + 4..])
            .expect("JSON-RPC request body");
        assert_eq!(requests[0]["jsonrpc"], "2.0");
        assert_eq!(requests[0]["method"], "gettxout");
        assert_eq!(requests[0]["params"], json!(["txid", 0, false]));
        assert_eq!(requests[0]["id"], 1);
    }

    #[test]
    fn http_client_rejects_announced_body_over_limit_before_decode() {
        let response = br#"[{"jsonrpc":"2.0","result":null,"id":1}]"#;
        let (address, _request, server) = serve_once(response, Duration::ZERO);
        let client = ClassificationRpcClient::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            Duration::from_secs(2),
            8,
        );

        let error = client
            .send_batch("gettxout", &[json!(["txid", 0, false])])
            .expect_err("oversized body");
        server.join().expect("fixture server");

        assert!(matches!(
            error,
            ClassificationRpcError::ResponseTooLarge {
                actual,
                maximum: 8
            } if actual == response.len()
        ));
    }

    #[test]
    fn http_client_handles_close_delimited_body_boundaries() {
        let valid_response = br#"[{"jsonrpc":"2.0","result":null,"id":1}]"#;
        for (case, response, maximum, accepted) in [
            (
                "exact limit",
                valid_response.as_slice(),
                valid_response.len(),
                true,
            ),
            ("limit plus one", b"123456789".as_slice(), 8, false),
        ] {
            let (address, _request, server) =
                serve_once_without_content_length(response, Duration::ZERO);
            let client = ClassificationRpcClient::new(
                &format!("http://{address}/"),
                "atlas".to_owned(),
                "secret".to_owned(),
                Duration::from_secs(2),
                maximum,
            );

            let result = client.send_batch("gettxout", &[json!(["txid", 0, false])]);
            server.join().expect("fixture server");

            if accepted {
                let batch = result.unwrap_or_else(|error| panic!("{case}: {error}"));
                assert_eq!(batch.response_bytes, response.len(), "{case}");
                assert!(
                    matches!(
                        batch.responses[0]
                            .as_ref()
                            .map(ClassificationRpcResponse::outcome),
                        Some(ClassificationRpcOutcome::Null)
                    ),
                    "{case}"
                );
            } else {
                assert!(
                    matches!(
                        result,
                        Err(ClassificationRpcError::ResponseTooLarge { actual, maximum: limit })
                            if actual == response.len() && limit == maximum
                    ),
                    "{case}"
                );
            }
        }
    }

    #[test]
    fn http_client_rejects_premature_eof_before_declared_length() {
        let response = br#"[{"jsonrpc":"2.0","result":null,"id":1}]"#;
        let expected = response.len() + 10;
        let (address, _request, server) =
            serve_once_configured(200, response, Duration::ZERO, Some(expected), String::new());
        let client = ClassificationRpcClient::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            Duration::from_secs(2),
            1024,
        );

        let error = client
            .send_batch("gettxout", &[json!(["txid", 0, false])])
            .expect_err("truncated response");
        server.join().expect("fixture server");

        assert!(matches!(
            error,
            ClassificationRpcError::IncompleteResponseBody {
                expected: declared,
                actual
            } if declared == expected && actual == response.len()
        ));
    }

    #[test]
    fn http_client_rejects_chunked_framing_even_when_complete() {
        let response = br#"[{"jsonrpc":"2.0","result":null,"id":1}]"#;
        let mut chunked = format!("{:x}\r\n", response.len()).into_bytes();
        chunked.extend_from_slice(response);
        chunked.extend_from_slice(b"\r\n0\r\n\r\n");
        let (address, _request, server) = serve_once_configured(
            200,
            &chunked,
            Duration::ZERO,
            None,
            "Transfer-Encoding: chunked\r\n".to_owned(),
        );
        let client = ClassificationRpcClient::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            Duration::from_secs(2),
            1024,
        );

        let error = client
            .send_batch("gettxout", &[json!(["txid", 0, false])])
            .expect_err("chunked response");
        server.join().expect("fixture server");

        assert!(matches!(
            error,
            ClassificationRpcError::UnsupportedTransferEncoding
        ));
    }

    #[test]
    fn http_client_does_not_follow_redirects() {
        let redirect_target = TcpListener::bind("127.0.0.1:0").expect("redirect target");
        redirect_target
            .set_nonblocking(true)
            .expect("non-blocking redirect target");
        let location = format!(
            "http://{}/capture",
            redirect_target.local_addr().expect("redirect address")
        );
        let (address, _request, server) = serve_once_configured(
            302,
            b"",
            Duration::ZERO,
            Some(0),
            format!("Location: {location}\r\n"),
        );
        let client = ClassificationRpcClient::new(
            &format!("http://{address}/"),
            "private-user".to_owned(),
            "private-password".to_owned(),
            Duration::from_secs(2),
            1024,
        );

        let error = client
            .send_batch("gettxout", &[json!(["txid", 0, false])])
            .expect_err("redirect response");
        server.join().expect("fixture server");

        assert!(matches!(
            error,
            ClassificationRpcError::UnexpectedHttpStatus(302)
        ));
        assert!(matches!(
            redirect_target.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn http_client_rejects_non_success_status_even_with_valid_json() {
        let response = br#"[{"jsonrpc":"2.0","result":null,"id":1}]"#;
        let (address, _request, server) = serve_once_with_status(500, response, Duration::ZERO);
        let client = ClassificationRpcClient::new(
            &format!("http://{address}/"),
            "private-user".to_owned(),
            "private-password".to_owned(),
            Duration::from_secs(2),
            1024,
        );

        let error = client
            .send_batch("gettxout", &[json!(["txid", 0, false])])
            .expect_err("HTTP failure");
        server.join().expect("fixture server");

        assert!(matches!(
            error,
            ClassificationRpcError::UnexpectedHttpStatus(500)
        ));
        let message = error.to_string();
        assert!(!message.contains("private-user"));
        assert!(!message.contains("private-password"));
    }

    #[test]
    fn http_client_applies_its_timeout() {
        let response = br#"[{"jsonrpc":"2.0","result":null,"id":1}]"#;
        let (address, _request, server) = serve_once(response, Duration::from_secs(2));
        let client = ClassificationRpcClient::new(
            &format!("http://{address}/"),
            "atlas".to_owned(),
            "secret".to_owned(),
            Duration::from_secs(1),
            1024,
        );

        let error = client
            .send_batch("gettxout", &[json!(["txid", 0, false])])
            .expect_err("timed out request");
        server.join().expect("fixture server");

        assert!(matches!(error, ClassificationRpcError::Transport(_)));
    }

    fn serve_once(
        response: &[u8],
        delay: Duration,
    ) -> (SocketAddr, Receiver<Vec<u8>>, JoinHandle<()>) {
        serve_once_with_status(200, response, delay)
    }

    fn serve_once_with_status(
        status_code: u16,
        response: &[u8],
        delay: Duration,
    ) -> (SocketAddr, Receiver<Vec<u8>>, JoinHandle<()>) {
        serve_once_configured(
            status_code,
            response,
            delay,
            Some(response.len()),
            String::new(),
        )
    }

    fn serve_once_without_content_length(
        response: &[u8],
        delay: Duration,
    ) -> (SocketAddr, Receiver<Vec<u8>>, JoinHandle<()>) {
        serve_once_configured(200, response, delay, None, String::new())
    }

    fn serve_once_configured(
        status_code: u16,
        response: &[u8],
        delay: Duration,
        content_length: Option<usize>,
        extra_headers: String,
    ) -> (SocketAddr, Receiver<Vec<u8>>, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let response = response.to_vec();
        let (request_sender, request_receiver) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixture connection");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("fixture read timeout");
            let request = read_http_request(&mut stream);
            request_sender.send(request).expect("capture request");
            thread::sleep(delay);
            let content_length = content_length
                .map(|length| format!("Content-Length: {length}\r\n"))
                .unwrap_or_default();
            let head = format!(
                "HTTP/1.1 {status_code} Fixture\r\nContent-Type: application/json\r\n{content_length}{extra_headers}Connection: close\r\n\r\n",
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&response);
        });
        (address, request_receiver, server)
    }

    fn read_http_request(stream: &mut impl Read) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];
        let mut expected_length = None;
        loop {
            let read = stream.read(&mut buffer).expect("fixture request read");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if expected_length.is_none()
                && let Some(header_end) =
                    request.windows(4).position(|window| window == b"\r\n\r\n")
            {
                let body_start = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                expected_length = Some(body_start.saturating_add(content_length));
            }
            if expected_length.is_some_and(|length| request.len() >= length) {
                break;
            }
        }
        request
    }
}
