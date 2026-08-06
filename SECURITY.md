# Security policy

## Reporting a vulnerability

Please do not disclose a suspected vulnerability in a public issue. Report it
privately through GitHub's security advisory interface for this repository.
Include the affected revision, reproduction steps, impact, and any suggested
mitigation. You should receive an acknowledgement after the report is reviewed.

## Scope

Mempool Atlas is a read-only viewer, but its Bitcoin RPC credential can invoke
anything allowed by the node's RPC whitelist. Deploy it only on a trusted host,
bind Atlas to loopback, keep RPC on a private network, and use the restricted
method list documented in the README. Never expose node credentials to the
browser or commit them to the repository.

The supported public boundary is Cloudflare Tunnel to the loopback Atlas
listener. Keep the origin firewall closed, restrict public methods to `GET` and
`HEAD`, and enable Cloudflare WAF and route-specific rate limits before launch.
Do not expose operational health routes publicly. The complete procedure is in
[`docs/deployment-cloudflare.md`](docs/deployment-cloudflare.md).

Only the current default branch receives security fixes.
