# Deploy behind Cloudflare

Mempool Atlas is a persistent origin service, not a Cloudflare Worker or Pages
application. Run Atlas beside `cloudflared` on the presentation host and expose
only the tunnel hostname.

```mermaid
flowchart LR
    browser["Public browser"] --> edge["Cloudflare edge<br/>TLS, cache, WAF, rate limits"]
    edge --> tunnel["Cloudflare Tunnel"]
    tunnel --> atlas["Atlas<br/>127.0.0.1:3101"]
    atlas --> nodes["Private Bitcoin RPC sources"]
```

Browser traffic never initiates Bitcoin RPC. Atlas continuously polls and
classifies every configured source even when nobody is viewing it.

## Deployment inputs

Record these values in the deployment system outside this repository before
installation:

- the Cloudflare zone and public hostname;
- the Cloudflare plan and features available to that zone;
- the tunnel UUID and credential location;
- the presentation-host paths and service account;
- the private RPC source inventory and credentials; and
- launch budgets for memory, poll duration, classification lag, response size,
  origin request rate, and outbound traffic.

Do not commit those values to this repository.

## Build the origin

Build and verify the release from a clean, reviewed revision:

```bash
npm --prefix web ci
just lint
just test
VITE_UMAMI_SCRIPT_URL=https://atlas.example.com/analytics/script.js \
VITE_UMAMI_WEBSITE_ID=00000000-0000-4000-8000-000000000000 \
VITE_UMAMI_DOMAINS=atlas.example.com \
  just build
cargo build --release --locked
```

Omit the three `VITE_UMAMI_*` variables when analytics is not required. By
default, reverse-proxy the Umami script and collection endpoint through the
Atlas hostname, as represented by `/analytics/` above. This keeps both
`script-src` and `connect-src` at `'self'`.

Only load analytics directly from a separate origin after explicitly trusting
that origin with the Atlas page. Add its exact HTTPS origin to both directives.
A script origin admitted by `script-src` runs with page privileges; a
compromised or defective trusted analytics script can block the browser main
thread indefinitely and stall Atlas interactions. CSP allowlisting grants
trust, not execution isolation.

For the example systemd layout, install the release binary and `web/dist` as one
release unit at `/opt/mempool-atlas`, matching the supplied paths. For atomic
upgrades, make that path a release pointer to a separately staged versioned
directory. Create a dedicated `mempool-atlas` system account with no interactive
shell. Copy
[`deploy/systemd/mempool-atlas.service.example`](../deploy/systemd/mempool-atlas.service.example)
to the host's systemd unit directory and review every path before enabling it.

Copy [`deploy/systemd/atlas.env.example`](../deploy/systemd/atlas.env.example)
to `/etc/mempool-atlas/atlas.env`. The environment file contains paths and
limits, not passwords. Store `sources.json` and the named password files under
`/etc/mempool-atlas`, owned by root and readable only by the Atlas service
group. Keep the source file, environment file, and password files out of Git.

The example unit deliberately leaves `MemoryHigh` and `MemoryMax` unset. Add
host-specific values only after measuring representative one, two, and
four-source workloads. An unverified memory ceiling can terminate Atlas during
a valid large snapshot and is not a substitute for the application bounds.

Verify the installed unit:

```bash
sudo systemd-analyze verify /etc/systemd/system/mempool-atlas.service
sudo systemctl enable --now mempool-atlas.service
curl --fail --silent http://127.0.0.1:3101/healthz
curl --fail --silent http://127.0.0.1:3101/api/v2/sources
```

`/readyz` becomes ready after any configured source has published a valid
snapshot. Monitor `/api/v2/sources` locally when every source must be checked.
The response's `atlas_version` must match the release being installed, and the
same value appears beside the Mempool Atlas name on both browser pages.

## Create the tunnel

Install `cloudflared` from Cloudflare's supported packages. Create a named
tunnel and public hostname, then copy
[`deploy/cloudflare/cloudflared-config.yml.example`](../deploy/cloudflare/cloudflared-config.yml.example)
to `/etc/cloudflared/config.yml` on the origin host.

Replace the tunnel UUID and hostname locally. The two operational health paths
are matched before the public application route and return `404`. The final
catch-all rule is mandatory and prevents an unmatched hostname from reaching
Atlas.

Validate and start the tunnel:

```bash
cloudflared tunnel ingress validate
cloudflared tunnel ingress rule https://atlas.example.com/
cloudflared tunnel ingress rule https://atlas.example.com/healthz
sudo systemctl enable --now cloudflared.service
```

Cloudflare documents the current locally managed configuration and validation
commands in its
[Tunnel configuration reference](https://developers.cloudflare.com/tunnel/advanced/local-management/configuration-file/).

The origin firewall should expose no Atlas port. `cloudflared` makes outbound
connections to Cloudflare and reaches Atlas over loopback. Confirm that neither
the host address nor any unproxied DNS record can reach port 3101.

## Configure cache behavior

Cloudflare does not cache HTML or JSON by default. Create explicit Cache Rules
for the public hostname and keep them narrow. Rule ordering matters because the
last matching rule wins.

The table below describes behavior derived from Atlas origin `Cache-Control`
headers and the route eligibility rules. The browser and edge columns are
outcomes, not Browser Cache TTL or Edge Cache TTL override values to enter in
Cloudflare.

| Route | Eligibility | Browser behavior | Edge behavior |
| --- | --- | --- | --- |
| `/assets/*` | Eligible | Long-lived and immutable | Long-lived |
| `/` and `/compare/` | Bypass | Revalidate | Bypass |
| `/api/v2/sources/*/mempool` | Eligible for `GET` and `HEAD` | Revalidate | Respect origin validators |
| `/api/v2/sources/*/mempool/stages/*` | Eligible for `GET` and `HEAD` | One year, immutable | One year; respect origin |
| `/api/v2/sources` | Bypass | `no-store` | Bypass |
| `/api/v2/sources/*/transactions/*` | Bypass | `no-store` | Bypass |
| errors and operational paths | Bypass | `no-store` | Bypass |

Atlas does not send `Cache-Control` for static documents or assets; the
browser and edge behavior for those two rows comes entirely from these Cache
Rules. Every API, operational, and error response carries an explicit origin
`Cache-Control` header.

For the manifest and stage rules:

1. Match only the Atlas hostname, `GET` or `HEAD`, and the exact source snapshot
   manifest or content-addressed stage path shapes.
2. Mark JSON eligible for cache.
3. Respect the origin `Cache-Control` and `ETag` headers. Do not set an Edge
   Cache TTL or Browser Cache TTL that overrides them.
4. Match only an empty query string. Requests with a non-empty query string
   must bypass the edge cache and reach Atlas, which rejects them as
   non-cacheable `400` responses. A bare trailing `?` can still satisfy an
   edge empty-query match and be forwarded, but Atlas rejects that request as
   a non-cacheable `400` too.
5. Do not enable stale serving for the API. Manifests must always revalidate;
   stages have their own explicit freshness lifetime and must revalidate after
   it expires. Atlas already publishes explicit source failure and staleness
   state.

Atlas returns `Cache-Control: public, no-cache, must-revalidate` and a weak
`ETag` for the current manifest. Never assign the manifest a freshness TTL: it
is the authority that declares the current stage IDs and must revalidate on
every reuse.

A successful current stage returns `Cache-Control: public,
max-age=31536000, immutable, must-revalidate` and a weak `ETag`. The content ID
in its URL is the SHA-256 digest of the exact body, so that URL is never reused
for different bytes. One year follows the established immutable-asset
convention; `must-revalidate` requires validation again after that freshness
lifetime. Cloudflare documents that `immutable` affects browsers rather than
public-cache freshness, while `max-age` supplies the edge and browser lifetime
when Origin Cache Control is respected.

Atlas checks current stage membership before evaluating a conditional header,
so an explicit matching `If-None-Match` request for a current representation
still returns `304` without the JSON body. A cached immutable stage represents
only the bytes named by its content ID, not evidence that a later manifest
still declares it. Clients must use stage IDs from the freshly revalidated
manifest.

A well-formed content identifier absent from the current publication returns
non-cacheable `409`. An identifier that belongs to a different current stage,
or a request for a stage kind or classifier that is not present, returns
non-cacheable `404`. A malformed identifier or stage kind returns non-cacheable
`400`, all before validator handling. Cache eligibility must not override those
`no-store` error responses. Before the first publication, the manifest and
stage routes return non-cacheable `503 application/problem+json` with no
validator.

Review Cloudflare's current
[default cache behavior](https://developers.cloudflare.com/cache/concepts/default-cache-behavior/),
[Cache Rule settings](https://developers.cloudflare.com/cache/how-to/cache-rules/settings/),
and [Origin Cache Control](https://developers.cloudflare.com/cache/concepts/cache-control/)
when creating the rules. Availability and minimum TTL behavior vary by plan.
Do not override the manifest with a fixed TTL or replace the stage lifetime
with an edge-only value merely to obtain a cache hit.

## Configure edge security

Enable the Cloudflare managed WAF rules available to the zone. Add a method rule
that permits only `GET` and `HEAD` for the public Atlas hostname.

Use the rate-limit rule capacity available to the zone plan. When the account
has only one shared rule, add an Atlas hostname arm to that rule rather than
prescribing unavailable per-route rules. The production arm matches
`atlas.example.com` with paths beginning `/api/v2/`, allows 50 requests per
IP per 10 seconds, and blocks for 10 seconds when exceeded.

A stable cold node publication load makes two manifest reads and seven stage
reads, for nine staged-publication requests. The first manifest plus the
population and selected-classifier stages make the primary view interactive;
the second manifest precedes the complete seven-stage publication, with the two
already loaded stages reused in the browser. A stable cold comparison performs
that flow for both sources concurrently: four manifest reads and fourteen stage
reads, for eighteen staged-publication requests. Source discovery is one
additional request per page load, and opening transaction detail adds another.

Treat those counts as the no-retry floor, not as the rate-limit threshold.
Every `429` request can be retried up to three times after its initial attempt,
and a superseded `409` can restart a whole-publication load up to three times;
unchanged content-addressed stages are reused where possible. The documented
50-request window admits a stable comparison with headroom, but five
simultaneous cold node loads consume 45 staged requests plus five
source-discovery requests before any detail, retry, or supersession. Choose the
production threshold from observed cold-burst traffic and origin capacity, and
do not count client retries as spare capacity. If the zone plan permits
additional rate-limit rules, use those rules to apply stronger limits to the
population and membership stages because they carry the largest bandwidth and
slow-reader cost. On a plan limited to one shared rule, keep those stages under
the hostname-wide rule described above.

Validate the rule in staging before enabling a blocking action. Cloudflare's
[rate-limiting documentation](https://developers.cloudflare.com/waf/rate-limiting-rules/)
describes the fields and actions available to each plan.

Add these response headers at the edge:

```text
Content-Security-Policy: default-src 'self'; base-uri 'none'; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; img-src 'self' data:; object-src 'none'; script-src 'self'; style-src 'self'; worker-src 'self'
Cross-Origin-Opener-Policy: same-origin
Permissions-Policy: camera=(), geolocation=(), microphone=()
Referrer-Policy: no-referrer
X-Content-Type-Options: nosniff
```

Enable HSTS after confirming that the selected hostname and any included
subdomains are HTTPS-only. Do not add `includeSubDomains` without checking the
whole parent domain.

Enable compression and verify that full JSON responses have a supported
`Content-Encoding`. Cloudflare lists `application/json` among its compressible
content types in the
[compression reference](https://developers.cloudflare.com/speed/optimization/content/compression/).

## Capacity and monitoring

The configured classification cache is a service-wide budget divided among
sources. Domain snapshots, classification detail maps, encoded v2 bundles, RPC
buffers, allocator overhead, and response buffers retained by slow readers are
outside that budget.

Atlas logs `publication_id`, `encoded_bytes`, and `classification_revision`
whenever it promotes a publication. Combine those fields with the existing
poll-round and classification logs.

Each changed stage content ID creates a new cacheable URL, but only requested
objects occupy an edge cache. The one-year freshness lifetime is not a
retention guarantee: Cloudflare documents that standard edge retention depends
on relative popularity and cache size, with least-recently-used eviction.
Routine purging of superseded content-addressed stages is not required for
correctness and unnecessarily discards useful hits; reserve purges for release
boundaries, rollback, or an explicit incident. Use Cache Analytics, when the
selected plan provides it, to review cache status, data transfer, and top stage
URLs. If Cache Reserve is enabled separately, monitor its persistent storage,
operations, and retention policy as a distinct cost surface. See Cloudflare's
[retention versus freshness](https://developers.cloudflare.com/cache/concepts/retention-vs-freshness/)
and [Cache Analytics](https://developers.cloudflare.com/cache/performance-review/cache-analytics/)
references.

Monitor:

- resident and peak memory;
- full poll-round and per-source collection duration;
- observation skew between configured sources;
- classification state, remaining work, and pauses;
- encoded publication size by source and revision;
- Cloudflare cache status, stage-URL churn, data transfer, and origin request
  rate;
- response bandwidth and compression ratio;
- rate-limit and WAF actions; and
- tunnel and Atlas service restarts.

Exercise one, two, and four sources with representative mempool sizes before
raising the deployed source count. Keep the hard maximum at four until all
launch budgets pass under the intended host and Cloudflare plan.

## Public smoke test

Run the smoke test only after at least one source has a ready snapshot and the
Cloudflare rules are active. The host running it must provide `awk`, `cp`,
`curl`, `grep`, `head`, `jq`, `mktemp`, `openssl`, `rm`, and `xxd` on `PATH`:

```bash
just smoke-public https://atlas.example.com node-a
```

It verifies the source discovery status, cache policy, and required semantic
Atlas version, plus hidden public health paths, Cloudflare routing,
compression, the always-revalidated manifest, every immutable declared stage,
manifest and stage validators, exact non-cacheable stage routing and currentness
errors, transaction detail when the publication is non-empty, and conditional
`304` behavior.
Rate-limit actions and direct-origin
isolation require separate staging and firewall checks.

## Release deployment

The application package exposes `/api/v2` as its single public API contract.
The edge configuration must contain no Atlas route, redirect, transform, cache
rule, or rate-limit arm for any other API version.

Treat the server binary and `web/dist` as one release unit built from the same
reviewed application revision. A package manager, immutable image, or versioned
installation directory can provide that unit. If using the example `/opt`
layout above, stage each release in its own directory and atomically repoint
`/opt/mempool-atlas`. Otherwise switch the service's binary and
`ATLAS_WEB_ROOT` together. Never replace the live server and browser artifacts
independently.

Use this deployment-agnostic release sequence:

1. Pin the exact reviewed application revision in the operator's deployment
   configuration. Record the expected Atlas version and verify that both the
   server and browser build use that same pin.
2. Build the complete release unit from a clean tree. Run the repository lint,
   test, and production build gates before accepting the artifact. Evaluate and
   build every affected host or image without switching live services yet.
3. Preflight the Cloudflare configuration, including the v2 manifest and stage
   cache rules, no-stale behavior, CSP worker allowance, method restriction,
   WAF policy, rate limits, and hidden operational paths.
4. Add a temporary fail-closed edge rule for the public hostname. Confirm that
   the rule blocks public traffic before changing the origin.
5. Apply source-side RPC configuration only when the reviewed release changes
   it. Switch the presentation service last, atomically selecting the matched
   server binary and browser root from the staged release unit.
6. While the edge remains blocked, verify loopback source discovery, the
   expected Atlas version, the current manifest, every declared stage,
   transaction detail, cache headers, validators, and conditional responses.
7. Purge the public `/api/v2` cache namespace and any changed static assets.
   Recheck the edge rules, then remove the temporary block.
8. Run `just smoke-public` through the public hostname. Exercise sequential
   cold node loads, concurrent cold node loads, and a worst-case comparison
   load within the launch budgets before declaring the release complete.
9. Record the deployed application pin, release-unit identity, edge-rule
   revision, verification results, and rollback target in the operator log.

The edge and origin deployment control planes cannot change atomically. The
temporary block makes the gap fail closed instead of serving mixed generations.

Rollback is limited to another reviewed v2 revision with the same edge
contract. First restore the temporary edge block. Select the previously
recorded application pin and matched release unit, build or fetch it before
switching, apply only affected source-side configuration, and switch the
presentation service last. Restore the server executable and browser root
together.

Restore the matching Cloudflare cache and security rules, purge the
`atlas.example.com/api/v2` prefix, and verify the loopback v2 API. Only then
remove the edge block and rerun `just smoke-public` from this repository. An
origin rollback does not revert Cloudflare state by itself.

## Failure and rollback checks

Before launch, exercise these cases:

1. Restart Atlas and confirm the public API does not cache the exact
   `v2_unavailable` response before first publication.
2. Stop one Bitcoin RPC source and confirm the retained publication gets a new
   stale manifest while its declared content-addressed stages remain unchanged
   and other sources continue.
3. Restart `cloudflared` and confirm it reconnects without exposing the origin.
4. Purge a superseded stage URL, request it with its old matching validator,
   and confirm the origin currentness check returns non-cacheable `409` rather
   than `304`. Purging prevents a still-fresh immutable edge entry from
   satisfying this origin-specific check.
5. Send an old manifest ETag after a classification update and confirm the
   response is `200` with the new manifest.
6. Send current manifest and stage ETags explicitly and confirm `304` with no
   body and the route-appropriate cache policy.
7. Run controlled concurrent stage reads and confirm polling and
   classification remain within the recorded budgets.

To remove public access, disable the tunnel hostname or its DNS route first.
Then stop the origin service if required. Atlas retains no application state on
disk, so a restart always requires fresh source observations.
