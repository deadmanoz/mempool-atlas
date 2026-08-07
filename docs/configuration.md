# Configuration

Mempool Atlas reads a source file, separate RPC password files, and a small set
of command-line options or equivalent environment variables.

## Prerequisites

- Rust 1.88 or newer
- Node.js 20.19.x or 22.12 or newer
- npm 10 or newer
- [`just`](https://github.com/casey/just)
- One to four reachable Bitcoin JSON-RPC endpoints running Bitcoin Core 28.0
  or newer, or a Bitcoin Knots release based on it. Atlas speaks strict
  JSON-RPC 2.0, which older nodes do not answer in kind.

Install the web dependencies once:

```bash
npm --prefix web ci
```

## Source file

Copy the example and edit it locally:

```bash
cp config/sources.example.json config/sources.json
```

Each source has this shape:

```json
{
  "source_id": "node-a",
  "source_label": "Mainnet node A",
  "rpc_url": "http://127.0.0.1:8332/",
  "rpc_username": "atlas",
  "rpc_password_credential": "node-a.password"
}
```

`source_id` is the stable URL-safe identifier used by the API. The label is
displayed in the browser. `rpc_password_credential` names a file inside the
credentials directory; it is not a password.

The example configures two mainnet nodes on port 8332. The second RPC address
is a placeholder for a separate node; edit it, or remove the second source
entirely if you only run one node.

`config/sources.json` is local configuration and is ignored by Git, as are the
named `*.password` credential files. Do not commit RPC addresses, usernames,
credentials, or host inventory. `just lint` asserts these ignore rules, so a
regression fails the build rather than reaching a commit.

### All sources must observe the same network

Every configured source must be on the same Bitcoin network. Atlas compares
mempools across sources, and a comparison between, say, a mainnet node and a
testnet or signet node is meaningless: the two mempools describe unrelated
chains, so every transaction lands in a source-only region and no overlap it
reports carries any information.

Atlas does not currently verify this. It reads the chain tip for membership
validation but does not reject a mixed-network configuration, so keeping the
sources on one network is the operator's responsibility. A startup check that
compares the reported chain across sources is a possible future addition.

## Credentials

Create one password file for each configured source:

```bash
mkdir -p var/credentials
printf '%s\n' 'replace-with-rpc-password' > var/credentials/node-a.password
chmod 600 var/credentials/node-a.password
```

Each file must contain one non-empty password line. Atlas rejects credential
names that escape the configured directory.

## Environment

Copy the portable development defaults:

```bash
cp .env.example .env
```

| Variable | Required | Default | Purpose |
| --- | --- | --- | --- |
| `ATLAS_SOURCES_FILE` | yes | none | JSON source configuration |
| `ATLAS_CREDENTIALS_DIRECTORY` | yes | none | Directory containing named password files |
| `ATLAS_POLL_SECONDS` | no | `300` | Complete membership interval |
| `ATLAS_MAX_MEMPOOL_ENTRIES` | no | `200000` | Membership entry limit |
| `ATLAS_CLASSIFICATION_SLICE_ENTRIES` | no | `2048` | Candidate window size, from 1 to 8192 |
| `ATLAS_CLASSIFICATION_RPC_LANES` | no | `4` | Concurrent classification lanes, from 1 to 8 |
| `ATLAS_CLASSIFICATION_TOTAL_CACHE_MIB` | no | `256` | Service-wide script-cache budget, at most 512 MiB |
| `ATLAS_BIND` | no | `127.0.0.1:3101` | Loopback API and website listener |
| `ATLAS_WEB_ROOT` | no | `web/dist` | Built website directory |

The same settings are available as command-line options. Run
`cargo run -- --help` for their names.

### Optional website analytics

The browser loads no analytics by default. A production build can enable a
self-hosted Umami tracker with build-time Vite variables:

| Variable | Required when enabled | Purpose |
| --- | --- | --- |
| `VITE_UMAMI_SCRIPT_URL` | yes | HTTPS URL of the Umami tracker script |
| `VITE_UMAMI_WEBSITE_ID` | yes | Umami website UUID |
| `VITE_UMAMI_DOMAINS` | no | Comma-separated hostnames accepted by the tracker |

Set both required values in the environment that runs `just build`. Use
`VITE_UMAMI_DOMAINS` in public deployments so the website ID only records the
intended hostnames. Atlas rejects an insecure URL, malformed UUID, or malformed
hostname list and leaves analytics disabled. These values are embedded in the
built browser assets and are not secrets.

## Bitcoin RPC permissions

Use a dedicated RPC identity and restrict it server-side to the methods Atlas
needs:

```text
getblockchaininfo
getmempoolinfo
getrawmempool
getrawtransaction
gettxout
```

Atlas's RPC clients are built without HTTPS support. Use `http://` only over
loopback or an otherwise encrypted, trusted private transport. HTTP Basic Auth
credentials are base64-encoded, not encrypted.

Atlas refuses an `rpc_url` that embeds credentials (`http://user:pass@host/`).
Credentials belong in `rpc_username` and the referenced password file, never
in the URL. RPC requests never follow redirects, so the configured credential
is only ever presented to the configured origin, and every response is read
through an explicit byte cap derived from `ATLAS_MAX_MEMPOOL_ENTRIES`.

## Build and run

```bash
just build
just dev
```

Atlas serves the API and built website from <http://127.0.0.1:3101> by default.
It intentionally refuses a non-loopback bind. The supported public deployment
uses Cloudflare Tunnel to reach this loopback listener. Cloudflare supplies TLS,
compression, cache rules, WAF, and request limits while the origin remains
unaddressable. See [Deploy behind Cloudflare](deployment-cloudflare.md).

For frontend development, keep `just dev` running and start Vite separately:

```bash
just web-dev
```

The browser refresh control reads the latest in-memory observation. It does not
request an immediate Bitcoin RPC poll.

After a source publishes its first snapshot, its manifest returns an `ETag` and
`Cache-Control: public, no-cache, must-revalidate`. Each successful
content-addressed stage returns `Cache-Control: public, max-age=31536000,
immutable, must-revalidate`; its SHA-256 URL is never reused for different
bytes. Atlas checks that a requested stage belongs to the current publication
before applying a conditional validator. A matching explicit `If-None-Match`
request therefore still returns `304` without a body. Waiting responses, source
discovery, transaction detail, superseded or unknown stages, other errors, and
operational endpoints remain `no-store`.
