# Transaction classification

Mempool Atlas classifies current transactions through independent lenses. A
lens answers one bounded question and never attempts to infer a single
universal transaction type. The same transaction can therefore carry exact
structural labels, one or more heuristic shape labels, data-protocol
fingerprints, and a source-local policy assessment at the same time.

This specification is original to Mempool Atlas. The transaction-properties
vocabulary is intended to be familiar to users of public mempool explorers,
but its rules and implementation are defined here. The data-carrier model is
informed by the public [WE HODL BTC methodology][we-hodl-methodology]. No
third-party classifier implementation is incorporated.

[we-hodl-methodology]: https://www.wehodlbtc.com/observatory/learn/methodology

## Public classifier contract

Every Atlas build publishes a classifier catalog. A descriptor declares:

- a stable classifier ID and independently versioned implementation;
- whether the lens is exact, heuristic, a fingerprint, or policy evaluation;
- whether labels are overlapping or a rule set;
- the facts required by the classifier; and
- the complete label vocabulary and user-facing descriptions.

Each classified transaction contains a compact result for every classifier
that ran. A result records its classifier ID, `complete` or `partial` state,
all matching label keys, an optional primary label, and bounded missing-fact
keys. Detailed evidence is retained only in the transaction-detail record so
the full snapshot remains bounded.

An absent result means the classifier has not produced a current result for
that exact `txid` and `wtxid`. A `partial` result preserves observations that
are already proven while identifying facts that were unavailable. Neither
state permits Atlas to fabricate a negative result.

The version 1 lenses publish as one atomic result set after the shared fact
resolver reaches a terminal state for that transaction. A compact snapshot
therefore contains every catalog result for a classified witness variant, or
no results for a variant whose classifier work has not completed.

Classifier versions change whenever a rule, threshold, precedence rule, or
evidence interpretation changes. Label keys remain stable within a version.

## `transaction_properties` version 1

This exact, multi-label lens describes transaction bytes and every available
spent-output script. It does not infer intent.

### Version and sequence

Exactly one of `version_1`, `version_2`, `version_3`, or `version_other` is
emitted from the serialized transaction version. `signals_rbf` is emitted when
at least one input sequence is below `0xfffffffe`, the explicit BIP-125
signalling boundary.

### Witness structure

`has_witness` is emitted when any input has a non-empty witness. A
`has_taproot_annex` label requires a spent P2TR output and a witness with at
least two elements whose final element begins with byte `0x50`. This is a
structural annex candidate, not a statement about script validity.

### Script families

The following labels are emitted when at least one known input prevout or
transaction output uses the family: `p2pk`, `bare_multisig`, `p2pkh`, `p2sh`,
`p2wpkh`, `p2wsh`, `p2tr`, `p2a`, `unknown_witness_program`, `op_return`, and
`unknown_script`.

Output scripts are always available from the raw transaction. Input families
depend on spent-output scripts. If one or more spent-output scripts are
unavailable, the result is `partial`, preserves labels proven by known scripts,
and reports `input_script_pubkeys` as missing. The absence of an input-family
label in a partial result is not a negative observation.

## `transaction_shape` version 1

This heuristic, multi-label lens describes shapes that are consistent with
common transaction purposes. Labels are deliberately prefixed or presented as
heuristics in the user interface. They are never proof of wallet ownership,
coordination, or intent.

### `possible_coinjoin`

All of the following must hold:

1. The transaction has at least five inputs and five outputs.
2. A non-zero output amount occurs at least three times.
3. No output scriptPubKey is repeated.
4. All input spent-output scripts are known and none is repeated.
5. No registered data-protocol fingerprint is present.

The result records the input count, output count, most repeated non-zero
amount, and repetition count. This rule favors a conservative, explainable
signal over broad protocol coverage. CoinJoin implementations with other
shapes will be missed, and unrelated transactions can still match.

### `consolidation`

The transaction has at least five inputs and at least five times as many
inputs as outputs. This label can match collaborative or service transactions
that are not wallet consolidation.

### `batch_payout`

The transaction has at least five outputs and at least five times as many
outputs as inputs. This label describes fan-out shape, not payment purpose.

When none of the three rules match, `other_shape` is emitted. If spent-output
scripts are unavailable, the result is `partial` because `possible_coinjoin`
could not be fully evaluated. The count-based rules can still be positively
reported.

## `data_protocols` version 1

This fingerprint, multi-label lens locates recognizable data-carrier patterns
in transaction bytes. It does not execute a protocol state machine or prove
that a payload is valid according to an external indexer. Multiple labels can
be emitted when multiple carriers occur.

### Witness carriers

- `inscription` identifies `OP_FALSE OP_IF <push "ord">` in an inferred
  Taproot leaf script. A raw witness-byte fallback may also identify the same
  opening and records lower-confidence evidence.
- `brc20` additionally requires an inscription body containing the
  whitespace-insensitive marker `"p":"brc-20"`. Every `brc20` result also
  includes `inscription`. Atlas does not validate the JSON document.

### Output carriers

- `runes` identifies an output beginning `OP_RETURN OP_13`.
- `stamps` identifies a structurally valid bare-multisig output with at least
  two 33-byte key pushes where one or more pushes do not begin with the
  compressed public-key prefixes `0x02` or `0x03`.
- `counterparty` identifies the ASCII marker `CNTRPRTY` in an OP_RETURN
  payload.
- `omni` identifies the ASCII marker `omni` in an OP_RETURN payload.
- `other_op_return` identifies an OP_RETURN output for which no registered
  OP_RETURN protocol fingerprint fired.

Runes, Counterparty, Omni, and other OP_RETURN are mutually exclusive for one
output in that precedence order. A transaction can still receive several of
these labels when separate outputs carry separate fingerprints. Historical
Counterparty and Omni encodings outside OP_RETURN are not detected.

`no_detected_protocol` is emitted only when no registered fingerprint fires.
It means no supported pattern was found, not that the transaction contains no
embedded data. The retained detection list is capped independently of
transaction size.

## `knots_bip110` version 1

This rule-set lens is the existing compatibility assessment against Bitcoin
Knots' deployed BIP-110 mempool policy. Its labels are `compatible`,
`violating`, and `indeterminate`. The result is partial whenever any rule has
missing facts, including when another rule already has proven violation
evidence. The existing seven-rule detail remains the authoritative evidence
surface for this lens.

Compatibility is not proof that a node accepted or relayed a transaction.
Violation is not consensus invalidity and is not proof of rejection.

## Known exclusions and extension boundary

Version 1 does not label CPFP, acceleration, replacement history, wallet or
service ownership, address reuse, mining-pool attribution, or external protocol
validity. Those claims require a source-local transaction graph, retained
history, external state, or entity attribution that the current per-transaction
fact set does not provide.

If Atlas later adds graph-derived properties such as ancestor or descendant
relationships, they belong in a separate versioned classifier. They must not be
folded into transaction shape or treated as inferred intent. A new classifier
is added to the catalog and transaction results atomically so older labels keep
their existing meaning.

## Presentation and comparison

The browser presents one classifier lens at a time through a shared selector.
That selection drives both the Classifications overview and the Buckets view.
Every lens remains visible in transaction-detail cards. Atlas never forms
Cartesian-product regions across classifiers.

For an exact, heuristic, or fingerprint lens, Buckets partitions transactions
by result coverage and a lens-specific presentation adapter. Complete,
partial, and unavailable results remain separate. Marginal label controls may
overlap, but each transaction still belongs to one terrain group.

`transaction_properties` uses five broad script profiles: Legacy / P2SH,
SegWit v0, Taproot, Multiple script groups, and Anchor / other scripts. Version,
RBF signalling, witness structure, OP_RETURN, and exact script-family labels do
not create more terrain groups. They remain exact per-transaction facts and
marginal controls. Other generic version 1 lenses retain exact observed
label-set buckets because their smaller vocabularies do not fragment the view.

The terrain preserves one selectable block per transaction inside its group.
Section, bucket, and transaction area remain proportional to transaction count
or virtual size, while density-aware spacing prevents the blocks from
overwhelming the grouping hierarchy. Labels appear only where their region can
display them cleanly, and marginal filters, samples, and transaction evidence
remain available through progressive disclosure. Classifier partitions are
cached for the current snapshot so selecting a group does not regroup and
resort the full mempool.

Selecting `knots_bip110` activates the specialist policy presentation. It uses
compatible, indeterminate, unavailable, exact violated-rule-set, and partial
proven-plus-unresolved-rule buckets. The seven rule controls and bounded rule
evidence remain available only for this lens.

Structural and data-protocol results belong to an exact witness variant.
Mempool relationship and policy results remain source-local. If two sources
report the same `txid` with different `wtxid` values, their classification
results are not merged.

The default node view is Classifications. Buckets follows the selected
classifier, while Fee rate by age remains independent of every classifier. The
comparison page remains a source-local policy matrix rather than combining
classifier taxonomies.

## Structure facts

Structure facts are exact per-transaction derivations, not a classifier lens.
They carry no labels and no heuristics, and they never influence classifier
results.

Membership facts arrive with every published membership row, verbatim from the
source node's verbose mempool entry: `weight`, `ancestor_count`,
`ancestor_vsize`, `ancestor_fee_sats`, `descendant_count`, `descendant_vsize`,
and `replaceable`. `ancestor_fee_sats` is the node's delta-adjusted ancestor
fee total including this transaction, so prioritisation deltas can move it
away from the raw fee sum; dividing it by `ancestor_vsize` yields the
effective package fee rate used for mining selection.
Ancestor and descendant counts include the transaction itself, matching the
node's reporting. `replaceable` is the node's effective BIP-125 view,
including inherited signaling; it is deliberately distinct from the exact
`signals_rbf` label, which reports explicit per-input signaling only.

Derived facts arrive progressively in the nullable `structure` object,
computed from the same raw transaction bytes fetched for classification:

| Fact | Definition |
| --- | --- |
| `input_count` | Number of transaction inputs |
| `output_count` | Number of transaction outputs |
| `op_return_bytes` | Sum of pushed payload bytes across all OP_RETURN output scripts |
| `output_sats` | Sum of all output values in satoshis |
| `witness_bytes` | Serialized total size minus base size |

`structure` is non-null exactly when classifier results are present for the
transaction, and it carries forward between snapshots only while the exact
`txid` and `wtxid` survive. Panels that consume derived facts state how many
transactions they cover rather than treating missing facts as zeros.
