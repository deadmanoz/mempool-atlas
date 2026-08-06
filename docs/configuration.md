# Configuration

Mempool Atlas reads a source file, separate RPC password files, and a small set
of command-line options or equivalent environment variables.

## Prerequisites

- Rust 1.88 or newer
- Node.js 20.19 or newer
- npm 10 or newer
- [`just`](https://github.com/casey/just)
- One to four reachable Bitcoin JSON-RPC endpoints

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
  "source_label": "Bitcoin node A",
  "rpc_url": "http://127.0.0.1:8332/",
  "rpc_username": "atlas",
  "rpc_password_credential": "node-a.password"
}
```

`source_id` is the stable URL-safe identifier used by the API. The label is
displayed in the browser. `rpc_password_credential` names a file inside the
credentials directory; it is not a password.

The source file is local configuration and is ignored by Git. Do not commit
RPC addresses, usernames, credentials, or host inventory.

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

Keep the RPC endpoint on a trusted network path. Atlas sends Basic Auth and
does not provide TLS termination for the Bitcoin RPC connection.

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

After a source publishes its first snapshot, the full snapshot endpoint returns
an `ETag` and `Cache-Control: public, no-cache, must-revalidate`. A matching
`If-None-Match` request returns `304` without a body. Waiting responses, source
discovery, transaction detail, errors, and operational endpoints remain
`no-store`.
