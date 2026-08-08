# Transaction classification

Mempool Atlas classifies current transactions through independent lenses. A
lens answers one bounded question and never attempts to infer a single
universal transaction type. The same transaction can therefore carry exact
structural labels, transaction and data-carriage shape heuristics,
data-protocol fingerprints, and a source-local policy assessment at the same
time.

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

Every catalog lens publishes as one atomic result set after the shared fact
resolver reaches a terminal state for that transaction. A compact snapshot
therefore contains every catalog result for a classified witness variant, or
no results for a variant whose classifier work has not completed.

A `partial` result may carry no labels at all. That is the honest reading when
a lens ran, one of its rules could not reach a terminal state, and no other
rule matched: nothing is proven either way. A `complete` result always names at
least one label.

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

## `transaction_shape` version 2

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

### `other_shape`

`other_shape` is a negative observation, so version 2 emits it only once every
registered heuristic has reached a terminal state and none of them matched. The
two count-based rules are always terminal because they read the raw
transaction. `possible_coinjoin` requires every spent-output script.

If spent-output scripts are unavailable, the result is `partial` and reports
`input_script_pubkeys` as missing. Count-based labels proven from the raw
transaction are still reported. If no rule matched either, the result carries
no labels at all rather than claiming that no shape applies. Version 1 emitted
`other_shape` in that case, which asserted a negative fact while a rule was
still unresolved.

## `data_protocols` version 3

This fingerprint, multi-label lens locates recognizable data-carrier patterns
in transaction bytes. It does not execute a protocol state machine or prove
that a payload is valid according to an external indexer. Multiple labels can
be emitted when multiple carriers occur.

### Deobfuscation

Counterparty and Bitcoin Stamps do not write their markers in the clear.
Both obfuscate the embedded payload with ARC4, keyed by the transaction's
first input txid. Atlas therefore runs the keystream over candidate payload
bytes before comparing markers.

The key is the first input's txid in the byte order of its RPC display form,
which is the reverse of its consensus-serialized byte array. The cipher is a
local implementation used only to deobfuscate data-carrier bytes; Atlas never
relies on ARC4 for any security property.

### Witness carriers

- `inscription` identifies either the classic `OP_FALSE OP_IF <push "ord">`
  envelope or the RDTS-compatible bare `ord` push followed by data pushes
  balanced back to zero depth with `OP_DROP` and `OP_2DROP`. The latter accepts
  data pushes through 256 bytes and rejects a 257-byte push. Evidence records
  which framing matched. A raw witness-byte fallback may also identify the
  classic opening and records lower-confidence evidence.
- `brc20` additionally requires an inscription body containing the
  whitespace-insensitive marker `"p":"brc-20"`. Every `brc20` result also
  includes `inscription`. Atlas does not validate the JSON document.

### OP_RETURN carriers

- `runes` identifies an output beginning `OP_RETURN OP_13`.
- `counterparty` requires that ARC4-deobfuscating the decoded OP_RETURN
  payload yields `CNTRPRTY` at offset 0. This is the encoding Counterparty
  actually uses for OP_RETURN, so a plaintext marker anywhere in the payload
  no longer fires the label. Version 1 scanned for the plaintext marker, which
  matched no live traffic and could match incidental bytes.
- `omni` requires the four-byte Omni Class C marker `omni` at the start of the
  decoded OP_RETURN payload, matching the published Class C layout. Version 1
  matched the marker anywhere in the payload while describing it as a prefix.
- `other_op_return` identifies an OP_RETURN output for which no registered
  OP_RETURN protocol fingerprint fired.

Runes, Counterparty, Omni, and other OP_RETURN are mutually exclusive for one
output in that precedence order. A transaction can still receive several of
these labels when separate outputs carry separate fingerprints.

### Bare-multisig carriers

Bare-multisig carriers are read across the whole transaction, because both
protocols split one payload over several outputs and reassemble it in output
order.

`stamps` fires from either of two paths, and always requires one of the
markers `stamp:`, `STAMP:`, `stamps:`, or `STAMPS:` to appear in a
deobfuscated stream. The marker, not the shape of the keys, is the evidence.
Classic Stamps deliberately writes payload bytes into slots that look like
valid compressed keys. Version 1 asked the opposite question, requiring a key
that does not begin `0x02` or `0x03`, so it matched the wrong transactions.

The burn-key path requires 1-of-3 bare-multisig outputs whose third key slot
is a known Bitcoin Stamps burn key. Payload bytes come from the first two key
slots of each such output, each contributing its 33-byte slot without the
leading prefix byte and the trailing byte, concatenated in output order and
deobfuscated as one stream. The evidence records whether that stream also
contains `CNTRPRTY`, which distinguishes the Counterparty-transported form
from the pure form.

The envelope path applies the same marker test to a stream that the
Counterparty fingerprint below has already proven. Early Stamps predate the
burn-key convention and leave no burn key to key off, so the marker inside the
proven envelope is the only evidence they offer.

`counterparty` in a bare-multisig carrier extracts payload bytes from every
multisig output: 31 bytes from each of the first two slots of a three-slot
output, or a length-prefixed run from the second slot of a two-slot output. It
accepts the transaction when either the concatenation of all such outputs or
one output on its own carries `CNTRPRTY` in the clear at offset 0, or carries
it at offset 0 or 1 after ARC4 deobfuscation. Offset 1 accounts for the
one-byte chunk-length prefix that the multisig transport writes ahead of the
envelope. The plaintext form is retained here, unlike for OP_RETURN, because
the 2014-era two-slot transport really did write the marker in the clear.

Bitcoin Stamps rides on Counterparty issuance, so the two fingerprints overlap
by construction. Stamps is evaluated first because it is the more specific
test, but neither label is withheld because the other fired: a Stamps payload
inside a proven Counterparty envelope reports both. Reference data-carrier
implementations that assign exactly one protocol per transaction drop
Counterparty once Stamps matches; these are independent Atlas lenses with no
single winning protocol, so both proven observations are published. Pure
Stamps, which carries no `CNTRPRTY` envelope, receives `stamps` alone.

A burn-key carrier whose marker does not survive deobfuscation proves neither
protocol. Nothing is claimed for it.

`no_detected_protocol` is emitted only when no registered fingerprint fires.
It means no supported pattern was found, not that the transaction contains no
embedded data. The retained detection list is capped independently of
transaction size, and each detection retains a bounded sample of its carrier
output indexes.

### What this lens does not cover

- No message is parsed. Atlas does not decode Counterparty message types,
  issuance fields, or asset identifiers, and does not decode Stamps content,
  so `stamps` means "a Stamps marker survived deobfuscation", not "a valid
  SRC-20 or classic Stamps issuance".
- OLGA Stamps, which move the payload into P2WSH outputs instead of
  bare multisig, are not detected.
- Counterparty's Taproot reveal format carries a plaintext `CNTRPRTY` in
  witness data rather than in an OP_RETURN or multisig output. Atlas does not
  fingerprint it, so those transactions are not labelled `counterparty`.
- Omni Class B, which encodes packets into bare-multisig outputs alongside an
  Exodus address output, is not detected. Only the Class C OP_RETURN form is.
- A transaction with no inputs has no ARC4 key, so every deobfuscation-based
  fingerprint is silently unavailable for it.
- The Stamps marker is located anywhere in the deobfuscated stream rather than
  at a fixed field offset, because deployed transactions place it at several
  offsets. An unrelated Counterparty message whose text contains one of the
  markers would also match.

Version 3 adds the RDTS-compatible Ordinals push/drop framing. It scans the
complete revealed script and reports every valid classic and push/drop
envelope, so an earlier inscription cannot hide a later BRC-20 marker. It keeps
the existing `inscription` and `brc20` questions and label keys because only
the recognized wire representation changed.

## `data_carriage_shape` version 5

This heuristic, multi-label lens asks whether a transaction contains a
high-confidence bulk-carrier witness or output-field shape, independently of
protocol branding or BIP-110 policy outcome. It inspects every output directly
from the raw transaction and scripts revealed by known P2TR and P2WSH inputs.
P2WSH witness scripts must match the spent output's SHA256 commitment. For
P2TR inputs, Atlas checks the control-block shape but does not recompute the
Taproot commitment.

- `push_drop_witness` requires one contiguous, stack-neutral sequence made only
  of data pushes no larger than 256 bytes, push-number opcodes, `OP_DROP`, and
  `OP_2DROP`. The sequence must contain at least two pushed elements, carry at
  least 64 pushed bytes, and balance to zero depth without an underflow or an
  intervening opcode. This recognizes large push/drop carrier shapes without
  treating an ordinary single value cleanup as bulk carriage.
- `opcode_value_coding` requires OP_PLENTY's self-framing v2 form: seven
  consecutive `OP_5` opcodes, eight valid modulo-22 length nibbles, an even and
  fully present payload length, only registered encoding opcodes in the body,
  and one of the three defined footers. Atlas does not infer this label from a
  merely unusual opcode distribution.
- `p2wsh_envelope` requires a revealed P2WSH witness script whose SHA256
  commitment matches the spent output and whose complete instruction sequence
  is the JXL-n-hide grammar: `OP_1 OP_NOTIF`, exactly six 255-byte pushes, then
  `OP_ENDIF OP_1`. The same byte sequence in Tapscript does not match.
- `witness_argument_carrier` requires at least four preceding witness arguments
  of exactly 255 bytes each. The complete revealed P2WSH or P2TR script must
  consist only of `OP_DROP` and `OP_2DROP` operations that consume exactly
  those arguments, followed by `OP_1`.
- `output_key_carrier` requires OLGA's two-byte big-endian payload length to
  select exactly two or more consecutive equal-value P2WSH outputs. The
  declared payload must consume that complete run, and every unused byte in
  the final 32-byte program must be zero padding. An adjacent equal-value
  P2WSH output makes the run ambiguous and prevents the label.
- `off_curve_p2tr` requires a 34-byte P2TR output whose 32-byte program cannot
  be parsed as a secp256k1 x-only public key. That proves the output key is
  unusable, but it does not prove why those bytes were chosen.
- `embedded_file_magic` requires a strong registered file-format signature in
  the canonical raw transaction serialization. Version 5 recognizes PDF, the
  full PNG signature, GIF87a/GIF89a, WASM version 1, the JPEG XL container,
  JFIF JPEG, and common ISO-BMFF `ftyp` brands. The scanner retains only a fixed
  overlap window and the first matching byte offset; it never buffers another
  serialized transaction.
- `no_detected_carriage_shape` is emitted only when every input script is known
  and none of the seven registered heuristics fires.

The seven positive labels are shapes, not proof of intent, protocol validity, or
policy rejection. A partial result preserves a positive match while naming
`input_script_pubkeys` as missing. If a spent-output script is unavailable and
no positive match is proven, the lens emits no negative label.

Version 5 adds the bounded raw-transaction file-signature scan. Version 4 added
the exact witness-argument/drop correlation; version 3 added the exact
committed P2WSH conditional envelope; version 2 added the exact OLGA output-run
grammar and the off-curve P2TR test; version 1 contained only the two general
witness-resident labels. The lens still does not claim generic on-curve
output-key carriers, hash160 carriers, file validity from a signature alone,
short collision-prone gzip or generic JPEG magic, other witness-argument ratios
or item sizes, field steganography, signature channels, or commitments. The
lens does not estimate total carried bytes. The separate
`structure.recognized_carried_bytes` fact publishes only a conservative lower
bound justified by the recognized shapes and OP_RETURN measure.

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

No catalog lens labels CPFP, acceleration, replacement history, wallet or
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
Atlas never forms Cartesian-product regions across classifiers. Changing the
selected lens changes the classifier outcome shown for the selected transaction;
the inspector does not render a cross-lens card stack.

In Classifications, each declared label is an independent toggle. ANY matches
at least one selected label and ALL matches every selected label. ALL is
available when at least two labels are selected from a multi-label descriptor;
other classifier semantics normalize to ANY. Each matching transaction appears
once in the full query result, with complete and partial populations rendered
in separate Canvas sections. A proven label in a partial result remains
queryable, while unavailable results never match. Selecting a block by pointer
or keyboard opens only the transaction's membership facts and active-lens
result. The label set and match mode are canonical URL state.
In ALL mode, an unselected label is disabled when adding it would produce an
empty intersection. Selected labels remain enabled so an impossible query
restored from the URL can always be reduced, and the empty result explains how
to recover by removing a label or switching to ANY.

For an exact, heuristic, or fingerprint lens, Buckets partitions transactions
by result coverage and a lens-specific presentation adapter. Complete,
partial, and unavailable results remain separate. Marginal label controls may
overlap, but each transaction still belongs to one terrain group.

`transaction_properties` uses five broad script profiles: Legacy / P2SH,
SegWit v0, Taproot, Multiple script groups, and Anchor / other scripts. Version,
RBF signalling, witness structure, OP_RETURN, and exact script-family labels do
not create more terrain groups. They remain exact per-transaction facts and
marginal controls. The other generic lenses retain exact observed label-set
buckets because their smaller vocabularies do not fragment the view. A partial
result with no proven labels forms its own empty label-set bucket; it is not
merged with any bucket that names a label.

The terrain preserves one selectable block per transaction inside its group.
Section, bucket, and transaction area remain proportional to transaction count
or virtual size, while density-aware spacing prevents the blocks from
overwhelming the grouping hierarchy. Labels appear only where their region can
display them cleanly. Marginal filters remain available through progressive
disclosure, while the selected transaction's facts and evidence remain directly
visible in the inspector. The snapshot-wide count/vsize metric controls both
terrain area and distribution weighting. Classifier partitions are cached for
the current snapshot so selecting a group does not regroup and resort the full
mempool.

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

Cross-source conflicting-spend analysis is also not a classifier. It answers a
relationship question over two independent populations, so it stays in the
browser-derived comparison product and never adds a label to either source.
When chain tips differ and both classification lifecycles are terminal, the
user can explicitly load a bounded source-local input fingerprint index.
Fingerprint matches identify candidates only. Atlas reports a pair only after
full 36-byte outpoints compare equal, excludes the same transaction ID, and
states analyzed-row coverage for both sources. A match does not prove
replacement intent, replay protection, rejection, relay cause, or safety.

## Structure facts

Structure facts are exact per-transaction derivations, not a classifier lens.
They carry no labels and no heuristics, and they never influence classifier
results.

Membership facts arrive with every published membership row, normalized from
the source node's verbose mempool entry: `weight`, `ancestor_count`,
`ancestor_vsize`, `ancestor_fee_sats`, `descendant_count`, `descendant_vsize`,
and `replaceable`. `ancestor_fee_sats` is the node's delta-adjusted ancestor
fee total including this transaction, so prioritisation deltas can move it
away from the raw fee sum. It is a signed integer: a negative
`prioritisetransaction` delta can push the total below zero, and Atlas
preserves that value exactly. Dividing it by `ancestor_vsize` yields the ancestor
fee rate shown by Atlas. It is not Bitcoin Core's cluster mempool mining score.
Ancestor and descendant counts include the transaction itself, matching the
node's reporting. `replaceable` is the node's effective BIP-125 view,
including inherited signaling; it is deliberately distinct from the exact
`signals_rbf` label, which reports explicit per-input signaling only.

Derived facts arrive progressively in the nullable `structure` object,
computed from the same raw transaction bytes fetched for classification:

| Fact                       | Definition                                                                                                                                                                                                      |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `input_count`              | Number of transaction inputs                                                                                                                                                                                    |
| `output_count`             | Number of transaction outputs                                                                                                                                                                                   |
| `op_return_bytes`          | Sum across OP_RETURN outputs. When a tail decodes entirely to pushes, count the decoded pushed bytes; if decoding fails or any non-push opcode is present, count that output's serialized bytes after OP_RETURN |
| `recognized_carried_bytes` | `op_return_bytes` plus the non-OP_RETURN bytes justified by the recognized high-confidence carrier shapes described below                                                                                   |
| `output_sats`              | Sum of all output values in satoshis                                                                                                                                                                            |
| `witness_bytes`            | Serialized total size minus base size                                                                                                                                                                           |

`structure` is non-null exactly when classifier results are present for the
transaction, and it carries forward between snapshots only while the exact
`txid` and `wtxid` survive. Panels that consume derived facts state how many
transactions they cover rather than treating missing facts as zeros.

`recognized_carried_bytes` sums disjoint positive detections. It adds pushed
bytes in a qualifying push/drop script, the decoded OP_PLENTY payload length,
the six exact JXL-n-hide envelope pushes, exact 255-byte witness arguments
consumed by matching drops, the declared OLGA payload length, and 32 bytes for
each provably off-curve P2TR output key. Embedded file signatures add no bytes
because they can describe bytes already counted through another carrier. The
evidence display remains bounded, but that display limit does not truncate the
byte total.

This is a conservative recognized-carriage measure, not an estimate of all
hidden data. It deliberately misses encrypted, on-curve, fragmented, or
otherwise unrecognized channels, and it does not infer intent. The OP_RETURN
component retains the hybrid definition above, including its serialized-tail
fallback for malformed or non-push scripts. Positive facts can contribute while
a classifier result is partial; missing facts are never treated as zero when
the structure object itself is unavailable.

The Data carriage distribution plots this transaction-total fact, not
serialized script size and not a per-output policy limit. Its reference ticks
include the historical 40-byte and 80-byte pushed-data defaults and exact sizes
for recognized witness, JXL-n-hide, and OLGA carriers. The 80-byte tick explains
the related 83-byte serialized OP_RETURN script reference without placing that
different measurement on the axis. The axis extends to 512 KiB so large witness
carriers remain visible.
