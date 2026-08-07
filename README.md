# Mempool Atlas

Mempool Atlas shows the current mempool reported by one to four Bitcoin nodes.
It classifies each transaction through four independent lenses. Comparison
happens in the browser; the server never combines mempools.

![Classifier buckets rendered from deterministic fixture data](docs/assets/node-buckets.png)

## What it shows

| Lens | Question |
| --- | --- |
| Transaction properties | Which exact version, witness, replaceability, and script-family properties are present? |
| Transaction shape | Which conservative CoinJoin, consolidation, or batch-payout heuristics match? |
| Data protocols | Which supported inscription, token, or OP_RETURN byte patterns are present? |
| Knots BIP-110 | Would this witness variant violate Bitcoin Knots' deployed policy? |

Atlas also shows fee rate, age, virtual size, ancestor fee rate, ancestry,
replaceability, structure facts, and transaction detail. The browser builds
the distribution charts from the current snapshot.

Partial and unavailable results are reported separately, not counted as
negatives. The
[classification specification](docs/classification.md) defines the rules,
thresholds, evidence, and limits.

Policy compatibility is not consensus validity. A transaction that violates a
BIP-110 rule may still be consensus-valid, and absence from one sampled mempool
does not prove that a node rejected or filtered it.

## Snapshot distributions

![Composition, fee structure, ancestor fee rate, and fee-rate-by-size density](docs/assets/snapshot-distributions.png)

![Age, data carriage, input-output density, and mempool entanglement](docs/assets/snapshot-distributions-detail.png)

![Output value distribution](docs/assets/snapshot-distributions-value.png)

## Compare independent mempools

The comparison page fetches two snapshots and derives three regions in the
browser: present in both, observed only on the left, and observed only on the
right. It shows the collection windows and their sampling skew.

Both products render the lightweight current source metadata before their full
snapshot bodies arrive. The comparison page keeps its primary membership
workspace ahead of secondary distributions and the policy matrix, so those
derived panels cannot displace the interactive view as they populate.

Each side keeps its own witness variant and policy assessment. Presence or
absence describes the sampled mempools only. It does not prove that a node
accepted, rejected, filtered, or relayed a transaction.

![Membership overlap with source-local policy controls](docs/assets/comparison-membership.png)

![Side-by-side classifier composition and fee structure](docs/assets/comparison-distributions.png)

![Side-by-side fee-rate-by-size and age distributions](docs/assets/comparison-distributions-age.png)

![Side-by-side data carriage, input-output density, and entanglement](docs/assets/comparison-distributions-structure.png)

![Side-by-side output value and source-local policy outcomes](docs/assets/comparison-distributions-value.png)

![The complete source-local policy matrix](docs/assets/policy-comparison.png)

## Architecture

![Mempool Atlas architecture](docs/assets/architecture.png)

One Atlas process polls the configured Bitcoin RPC endpoints, publishes
complete membership, then fills in classifier results. It keeps only the
latest successful observation for each source in memory and serves both the
API and website.

For public deployment, Atlas remains bound to loopback behind Cloudflare
Tunnel. Cloudflare supplies the public TLS, compression, cache, WAF, and rate-
limit boundary without exposing Bitcoin RPC or the Atlas listener.

There is no database, retained history, node-side agent, or server-side
combined mempool. The editable diagram is
[`docs/assets/architecture.drawio`](docs/assets/architecture.drawio).

See [Architecture](docs/architecture.md) for the data flow and
[Configuration](docs/configuration.md) for setup.

## Quick start

You need Rust 1.88 or newer, Node.js 20.19.x or 22.12 or newer, npm 10 or newer,
and [`just`](https://github.com/casey/just). Your Bitcoin RPC endpoint must permit
`getblockchaininfo`, `getmempoolinfo`, `getrawmempool`, `getrawtransaction`,
and `gettxout`.

```bash
npm --prefix web ci
cp config/sources.example.json config/sources.json
cp .env.example .env
mkdir -p var/credentials
printf '%s\n' 'replace-with-rpc-password' > var/credentials/node-a.password
chmod 600 var/credentials/node-a.password
just build
just dev
```

Edit `config/sources.json` to match the RPC URL, username, label, and credential
filename for your node. Remove the second example source unless you also create
its credential file. Atlas listens on <http://127.0.0.1:3101> by default.

The example configures two mainnet nodes, and that is deliberate: every source
you configure must observe the same Bitcoin network. Comparing a mainnet
mempool against a testnet or signet one is meaningless, because the two
mempools describe unrelated chains. Atlas does not check this for you today.

`config/sources.json` and the `*.password` credential files are ignored by Git.
Keep RPC addresses, usernames, credentials, and host inventory out of commits.

Common commands:

```bash
just build
just test
just lint
just format
just clean
```

### Frontend development without a node

Run `just web-fixtures` in one terminal and `just web-dev` in another. The
command first exports three deterministic synthetic sources from the Rust
domain model, then serves their pre-encoded API bodies on
<http://127.0.0.1:3101>. `just test-web-e2e` regenerates the same small profile
automatically.

`just perf-web` builds the production website and measures it against a
separate 70,000-transaction, two-source profile under recorded desktop and
mobile conditions. Results are written to `web/.perf-results/latest.json`.
See [client performance](docs/client-performance.md) for the measurement
contract, reconciled baseline, and current checkpoints.

## API

- `GET /healthz` reports process health.
- `GET /readyz` becomes ready after the first valid snapshot.
- `GET /api/v2/sources` reports the running Atlas version and lists configured
  sources with their current status.
- `GET /api/v2/sources/{source_id}/mempool` returns the current publication
  manifest.
- `GET /api/v2/sources/{source_id}/mempool/stages/{kind}/{content_id}` returns a
  content-addressed population, membership, or structure stage.
- `GET /api/v2/sources/{source_id}/mempool/stages/classifier/{classifier_id}/{content_id}`
  returns one content-addressed classifier stage.
- `GET /api/v2/sources/{source_id}/transactions/{txid}` returns classifier and
  policy detail for one current transaction.

Source discovery, transaction detail, operational responses, and errors disable
caching. Published manifests and stages have `ETag` validators and require
revalidation, so an unchanged conditional request returns `304` without
transferring the JSON body. The browser refresh action reads Atlas' latest
in-memory publication; it does not trigger a Bitcoin RPC poll.

## Public deployment

Public deployment uses Cloudflare Tunnel to reach the loopback Atlas listener.
See [Deploy behind Cloudflare](docs/deployment-cloudflare.md) for setup and the
smoke test. A deployment can enable a self-hosted Umami tracker at build time;
analytics is disabled by default. Keep deployment credentials, hostnames,
private RPC addresses, and account identifiers out of this repository.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the development workflow and
[SECURITY.md](SECURITY.md) for reporting vulnerabilities.

## License

Mempool Atlas is available under the [MIT License](LICENSE). Vendored
third-party material and its upstream notices are listed in
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
