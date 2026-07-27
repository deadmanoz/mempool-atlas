# Own the policy JSON-RPC wire boundary

Status: accepted

## Context

Classification depends on a distinction that the `jsonrpc` 0.18 response model
does not preserve. It decodes both a present `result: null` and an omitted
`result` member to the same value. For `gettxout(txid, vout, false)`, only the
present null is authoritative no-value data. Treating a malformed omission as
the same result can manufacture a terminal missing script fact.

The old transport also exposed a batch only after the complete HTTP body had
been buffered and parsed. Atlas could estimate planned response size and count
a reserialized envelope afterward, but it could not enforce the 16 MiB body
limit while receiving data or measure the raw JSON body before parsing.

Classification runs several concurrent batch lanes. The boundary must also
allocate globally unique request IDs, restore out-of-order responses, retain
missing response slots, and reject ambiguous ID mappings without leaking RPC
credentials.

## Decision

Keep complete membership collection on `corepc-client`. Give classification a
private `policy_rpc.rs` module backed directly by lazy `minreq` HTTP reads.
This is an internal transport boundary, not a new process, service, listener,
or network path.

The policy client:

- sends Basic Auth to the existing private WireGuard RPC proxy with the existing
  20-second timeout and never follows redirects;
- bounds the response status line and headers, requires HTTP 200, and keeps
  credentials out of its debug and error text;
- rejects any `Transfer-Encoding`, rejects a declared body above 16 MiB before
  reading it, requires the received byte count to exactly match any declaration,
  and aborts a close-delimited body on the first byte beyond 16 MiB before JSON
  parsing;
- sends JSON-RPC 2.0 batches with globally unique numeric IDs, restores valid
  out-of-order responses to request order, retains absent expected slots, and
  rejects duplicate or unexpected IDs and excess responses;
- preserves `result` independently as missing, null, or value and preserves
  `error` presence until the required Bitcoin Core JSON-RPC 2.0 envelope shape
  is interpreted; and
- gives a valid RPC error precedence over any simultaneous result.

Policy scheduling remains in `policy.rs`. A valid null outcome becomes
`ExplicitNull` only for `gettxout`. An omitted result, explicit `error: null`,
malformed error object, ordinary RPC error, invalid payload, or absent response
never becomes evaluator input. Protocol and warm-up errors retain their
systemic circuit-breaker behavior. A transport, framing, body, or batch-mapping
failure pauses the generation through the existing batch-failure path.

The deployed node proxy already disables chunked transfer encoding. Deployment
acceptance must verify that it continues returning either declared-length or
close-delimited responses without `Transfer-Encoding`.

## Consequences

One classification response body is bounded before parsing. For each
successfully decoded and ID-reconciled batch, `response_bytes` reports the exact
JSON body length including whitespace. This is not a total process-memory bound.
Concurrent lane-local bodies, the separately buffered membership response,
pending transactions, caches, publication overlap, and allocator overhead can
coexist under the production memory cgroup.

A proxy that returns a transfer-encoded classification response is intentionally
incompatible and trips the batch circuit breaker. Close-delimited framing uses
connection EOF as the HTTP body terminator. Atlas still requires the bytes
before EOF to decode as one complete JSON array, so truncation before the closing
delimiter fails while loss of only trailing whitespace cannot change the batch
semantics. `minreq` exposes response headers in a map, so it cannot independently
detect duplicate `Content-Length` fields; the direct trusted Bitcoin Core and
nginx path remains the required deployment boundary.

## Alternatives considered

### Continue using `jsonrpc` 0.18

This cannot satisfy the semantic requirement because member presence is lost
before policy code receives the response. Its transport also cannot expose a
bounded raw body or exact wire-byte count through the client interface.

### Adapt or fork the `jsonrpc` transport

An adapter still receives the lossy response model. Maintaining a fork solely
to expose raw envelopes would add a larger long-term dependency surface than
the small private transport module while Atlas would still own HTTP bounds and
ID reconciliation.

### Accept transfer-encoded responses

The lazy `minreq` interface does not expose enough framing state to prove that
a chunked response observed its terminal chunk rather than an early EOF.
Rejecting transfer encoding is simpler to audit and matches the existing target
path, which currently uses close-delimited responses.
