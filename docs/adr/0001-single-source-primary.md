# Make one observed mempool the primary product model

Mempool Atlas models and visualizes one logical source as its primary product view. Each source snapshot owns its current membership, observation freshness, and capture integrity.

Shared storage may co-locate source projections for operational simplicity, but Atlas never constructs a combined cross-source mempool. A comparison workspace is an optional derived consumer of two or more independent source snapshots. Fork deployments may temporarily make that workspace the landing view without changing the underlying product or persistence model.

This keeps the long-lived "swimming in the mempool" experience simple while allowing short-lived fork analysis to remain explicit and removable.
