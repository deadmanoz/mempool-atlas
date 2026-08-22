# RDTS-compliant data carriers

Research notes on published techniques that pass all seven BIP-110 (RDTS /
`REDUCED_DATA`) transaction rules and the retained Knots datacarrier policy,
yet still carry arbitrary data.

**Status:** research input, not a specification. The implemented subset is
identified below; remaining techniques are research candidates or documented
limits. This document exists so classifier extensions can be designed and
reviewed against real, cited techniques rather than guesses.

**Compiled:** 2026-08-08.
**Implementation status updated:** 2026-08-09.

---

## 1. Why this document exists

Atlas already evaluates the seven RDTS rules. `src/bip110/` is a byte-exact
mirror of the enforcement client, and the `knots_bip110` lens publishes
`compatible`, `violating`, or `indeterminate` per transaction.

That lens answers "would Knots' deployed policy reject this?". It deliberately
does not answer "is this carrying data?". The two questions have drifted apart:
a public body of work now exists whose entire purpose is to be **`compatible`
under `knots_bip110` while carrying a payload**. Under the current catalog such
a transaction is labelled compatible by the policy lens and, in most cases,
`no_detected_protocol` by the `data_protocols` lens. Both labels are correct
under their own definitions and together they read as "clean".

The techniques below are the ones that produce that reading. Each entry is
written so a classifier author can go straight from the mechanism to a
detection signal without re-reading the sources.

### Scope

In scope: techniques where a transaction can pass **all seven rules** plus the
retained Knots OP_RETURN policy and still carry a payload.

Out of scope, and deliberately excluded: everything RDTS already catches. That
includes classic Ordinals `OP_FALSE OP_IF ... OP_ENDIF` tapscript envelopes
(rule 7, plus 520-byte pushes against rule 2), bare-multisig carriers such as
classic Bitcoin Stamps (about 105-byte outputs against rule 1, and
`permitbaremultisig=false`), and oversized OP_RETURN. Those are named only
where a technique is the direct descendant of one of them.

The rules themselves are not restated here. They live in `src/bip110/` and
`docs/classification.md`.

### One implementation detail that drives most of this document

Rule 2 caps pushed data elements and script-argument witness items at 256
bytes. It does **not** cap the revealed script blob itself, because that blob
is code rather than a data push. This is explicit in the evaluator:

- `src/bip110/rules/script_rules.rs:226` (`evaluate_p2wsh`) does
  `items.split_last()`, sends only the leading arguments through
  `witness_item_violations`, and sends the witnessScript through
  `push_violations`, which inspects pushes *decoded inside* it.
- The Taproot script-path branch treats the tapscript the same way.

So a multi-kilobyte witnessScript or tapscript is compliant as long as every
push **inside** it is 256 bytes or smaller. Nearly every high-capacity
technique in Section 4 is a way to exploit that asymmetry.

The second structural driver: **rules 6 and 7 are Tapscript-scoped.**
`evaluate_p2wsh` never reaches the `OP_SUCCESS` or `OP_IF` checks, which are
applied only after the `TAPROOT_LEAF_TAPSCRIPT` leaf-version dispatch in
`src/bip110/rules/script_rules.rs:302`. Moving a classic `OP_IF` envelope from
Tapscript down to P2WSH witness v0 therefore restores it, legally. Technique
W3 is exactly that move.

---

## 2. Technique summary

Ordered by descending payload capacity. "Capacity" is per transaction unless
noted.

| ID | Technique | Payload location | Capacity | Detectability |
| --- | --- | --- | --- | --- |
| [W1](#w1) | Push-drop tapscript envelope | Tapscript pushes | ~3.96 MB/block, ~99% dense | High |
| [W2](#w2) | OP_PLENTY opcode-value coding | Tapscript opcode choice | ~2 MB, 2 opcodes/byte | High |
| [W3](#w3) | P2WSH `OP_IF` envelope | v0 witnessScript | ~9.9 KB/input | High |
| [W4](#w4) | Witness-argument drop channel | Dropped witness stack items | ~255 KB/input | Medium |
| [W5](#w5) | Control-block steganography | Internal key + Merkle siblings | ~224 B/input | Low |
| [O1](#o1) | Fake P2TR output key | 32-byte output key | 32 B/output | Medium |
| [O2](#o2) | Fake P2WSH (OLGA / Stamps) | 32-byte script hash | 32 B/output, ~6.3 KB/tx | High |
| [O3](#o3) | Fake P2PKH / P2SH / P2WPKH | 20-byte hash160 | 20 B/output | Medium |
| [F1](#f1) | Transaction-field stego | `nSequence`, amounts, locktime | ~2 B/output | Low |
| [S1](#s1) | Signature and nonce channels | ECDSA `r`, Schnorr `R.x` | ~4 B/signature | Very low |
| [C1](#c1) | Key-path commitments | Taproot tweak | 32-byte commitment only | None |

An orthogonal overlay, [P1](#p1), applies to W1/W3/W4: crafting the payload so
the **raw transaction bytes are simultaneously a valid file**, which makes
naive file-carving recover it directly.

---

## 3. What Atlas detects today, and the remaining gaps

`data_protocols` version 3 recognizes the compliant bare-`ord` push/drop
envelope as the existing `inscription` and `brc20` protocol fingerprints.
`data_carriage_shape` version 5 independently recognizes balanced push/drop
witness runs, self-framed OP_PLENTY v2, the exact committed JXL-n-hide P2WSH
envelope, exact 255-byte witness-argument/drop channels, exact OLGA-style P2WSH
output runs, off-curve P2TR output keys, and strong file signatures in canonical
raw transaction bytes.

The remaining concrete gaps are:

1. **Witness-argument analysis is deliberately narrow.** Atlas recognizes only
   four or more 255-byte arguments consumed exactly by a drop-only script. It
   does not infer W4 from weaker push-to-logic ratios or other item sizes.

2. **Output-field analysis is deliberately narrow.** Atlas recognizes the
   self-consistent OLGA length framing and provably off-curve P2TR keys. It does
   not infer generic on-curve P2TR carriage or concatenate P2PKH, P2SH, or
   P2WPKH hash fields.

3. **The embedded-file scan is intentionally conservative.** It recognizes
   strong, format-specific headers. Bare two-byte gzip and generic three-byte
   JPEG markers are excluded because random transaction identifiers,
   signatures, and witness payloads make their mempool-wide collision rate too
   high. A signature is a fingerprint, not proof that the suffix is a valid
   file.

4. **Resolved: `structure.recognized_carried_bytes` now covers implemented
   carriers.** `structure.op_return_bytes` remains channel-specific, while the
   sibling fact adds bytes justified by the exact push/drop, OP_PLENTY,
   JXL-n-hide, witness-argument, OLGA, and off-curve P2TR fingerprints. The
   distribution now uses the broader fact, so recognized witness carriers no
   longer appear empty. This remains a conservative lower bound, not a total
   payload estimate.

---

## 4. Technique catalog

### Family W: witness-resident payloads

These carry bulk data. They are the ones that matter most.

<a id="w1"></a>
#### W1. Push-drop tapscript envelope (stack-neutral, no `OP_IF`)

**Mechanism.** A Taproot script-path reveal whose tapleaf pushes data chunks
and immediately discards them, so the stack returns to its starting depth:

```
<pubkey> OP_CHECKSIG  "ord"  <chunk0> <chunk1> OP_2DROP  <chunk2> <chunk3> OP_2DROP ... OP_DROP
```

Chunks are 255 bytes (`OP_PUSHDATA1`, 2 overhead bytes; 256 would force
`OP_PUSHDATA2` and 3). `OP_2DROP` discards two pushes with one byte, giving
`510 / 515` or about 99.0% density. Because the envelope is stack-neutral it
can be appended after a genuine spend condition, so the output is
authenticated rather than anyone-can-spend.

**Why it complies.** Rule 7 is avoided because there are no conditionals at
all: `OP_DROP`/`OP_2DROP` replace the `OP_FALSE OP_IF ... OP_ENDIF` framing.
Rule 2 is satisfied because every push is 255 bytes, and the tapleaf blob
itself is exempt as code. Leaf version stays `0xc0` (rule 3), a single leaf
gives a 33-byte control block (rule 5), there is no annex (rule 4), and the
alphabet contains no `OP_SUCCESS` (rule 6). The P2TR output is exactly 34
bytes (rule 1).

**Capacity.** BIP-342 removed both the 10,000-byte script-size cap and the
201-opcode cap, so the limit is block weight. `bip110-packer` measures
**3,958,620 bytes packed, 99.95% block fill, 99.019% efficiency**, all
transactions passing an independent BIP-110 check.

**Detection signals.**
- Long runs of `PUSHDATA (<=255 B) ... OP_DROP / OP_2DROP` in a revealed
  tapscript, with the pushes never consumed by real logic.
- Push-count and drop-count balance: the envelope pops exactly what it pushed.
- A bare `03 6f 72 64` (`OP_PUSHBYTES_3 "ord"`) push **not** preceded by
  `00 63`. This is the discriminator against the classic envelope and the
  direct fix for gap 1 above.
- Absence of `OP_IF`/`OP_NOTIF`/`OP_ENDIF` in the framing, and no stray opcode
  between the marker and its balancing drops.
- After stripping framing, the push list parses as an ordinary ord inscription
  (content-type tag, empty-push separator, body chunks), so the existing
  payload parser is reusable verbatim.

**False positives.** Low. Real scripts rarely push and discard kilobytes.
Note the envelope may sit anywhere in a larger tapleaf, several envelopes may
appear in one witness, and classic plus compliant envelopes can coexist, so
scan the whole leaf script rather than assuming position 0.

**Provenance.**
- [ordinals/ord#4545](https://github.com/ordinals/ord/pull/4545), "Add BIP-110
  compatible envelope parser", author **lifofifoX**, opened 2026-07-02, commit
  `26a7d5cc97cf968b850d182522810c5e7c357c99`, one file, +386/-64. **Open, not
  merged**: maintainer casey commented "Looks good to me! Let's wait until
  BIP-110 activates to merge this." Adds `BIP110_MAX_PUSH_SIZE = 256` and a
  stack-depth-counter parser, with 13 unit tests including 256-byte pass and
  257-byte fail boundaries.
- Announced by **lifofifo** on X, 2026-07-03, "Ordinals are now BIP-110 ready."
  <https://x.com/lifofifo/status/2072828299097555000>
- Parallel TypeScript parser:
  [ordpool-space/ordpool-parser#22](https://github.com/ordpool-space/ordpool-parser/pull/22)
  by hans-crypto, open.
- Independent maximal-density implementation:
  [MonumentalSystems/bip110-packer](https://github.com/MonumentalSystems/bip110-packer)
  (Richard J. Safier / Monumental Systems, created 2026-07-04, MIT,
  `bip110-packer` on crates.io), `tapleaf` channel. Its README cites ord#4545
  as the canonical envelope form.

<a id="w2"></a>
#### W2. OP_PLENTY: data in the choice of opcode

**Mechanism.** Rather than pushing data, each 4-bit nibble is encoded as *which*
tapscript opcode appears. Decoding is stateless: `nibble = opcode % 22`. Two
opcodes carry one byte. Encoding needs a small state machine because most
nibbles have two representatives, a "growing" one and a "shrinking" one, chosen
to keep a scratch-stack depth inside `{5, 6, 7}` so the script still executes
validly. The decoder never needs to reproduce that walk.

Framing is optional and comes in two forms:

- Unframed: seed `51 00 00 00 00 00 00` (`OP_1` then six `OP_0`), footer
  `6d 6d 61` / `6d 6d 75` / `6d 6d 6d` depending on exit depth.
- Self-framing v2: magic **`55 55 55 55 55 55 55`** (seven `OP_5`), then eight
  encoded length nibbles holding a four-byte big-endian count of payload hex
  characters, then the payload, then the footer.

The v2 magic makes the payload self-locating: search the raw transaction for
seven `OP_5` bytes, fold the next eight opcodes mod 22 to get the length, then
decode that many opcode-nibbles.

**Why it complies.** I verified the alphabet directly against the BIP-342
`OP_SUCCESS` set: **zero collisions**, and it contains no `0x63`/`0x64`. Every
symbol is a single-byte non-push opcode, so rule 2 cannot be tripped.
The gist notes the modulus 22 was chosen as the smallest that supplies all
sixteen nibble classes while avoiding disabled, data-dependent-failure, and
`OP_SUCCESS` slots.

**Capacity.** Up to 2 MB claimed, at 2 opcodes per payload byte. The author
describes it as "half the cost of OP_RETURN, zero PUSHDATA usage", consistent
with a 4x witness discount against a 2x encoding expansion.

**Detection signals.** This is the most mechanically detectable technique in
the document, because its alphabet is bizarre.
- Literal magic scan for `55 55 55 55 55 55 55` in witness bytes.
- Unframed seed `51 00 00 00 00 00 00`, and footers `6d 6d {61|75|6d}`.
- Statistical: a long contiguous run of opcodes drawn only from the alphabet
  below, with **zero pushes**. Real scripts of any length essentially always
  contain pushes (keys, signatures, hashes). A push-free tapscript of hundreds
  of bytes is by itself anomalous.
- Arithmetic and comparison opcodes (`OP_BOOLAND`, `OP_NUMNOTEQUAL`,
  `OP_GREATERTHAN`, `OP_LESSTHANOREQUAL`, `OP_MAX`, `OP_NEGATE`, `OP_ABS`)
  appearing in dense repeated runs.

**Full alphabet**, for a direct implementation. 28 of these are encoding
symbols (12 nibble pairs plus 4 depth-neutral unary opcodes); the remaining
three, `0x55` (v2 magic), `0x6d` and `0x75` (footers), are framing only:

```
0x51 0x55 0x58 0x59 0x5a 0x5b 0x5c 0x5d 0x5e 0x5f 0x60 0x61 0x6d 0x75 0x77 0x78
0x87 0x8f 0x90 0x91 0x92 0x93 0x9a 0x9b 0x9c 0x9e 0x9f 0xa0 0xa1 0xa2 0xa4
```

Because `0x6d` (`OP_2DROP`) is deliberately absent from the encoding alphabet,
it doubles as an unambiguous body terminator once the decoder knows where the
encoded region starts.

**False positives.** Very low given the magic plus the push-free property.

**Provenance.** **Steve Rabinow** (X [@steverabinow](https://x.com/steverabinow),
GitHub `stevenrabinow-hash`).
- Announcement, 2026-07-22:
  <https://x.com/steverabinow/status/2079613801976967450> ("BIP110 taketh away
  OP_IF, but giveth OP_PLENTY... Up to 2MB fully BIP110 compliant.")
- Reference implementation gist, created 2026-07-21, `OP_PLENTY.py`:
  <https://gist.github.com/stevenrabinow-hash/b71d7e085cb67a91b4553f750a1086dd>
  Contains the codec rationale, the worked `b"Hi"` example, the alphabet
  tables, and `encode`/`decode`/`asm` functions.

<a id="w3"></a>
#### W3. P2WSH `OP_IF` envelope: the same trick one witness version down

**Mechanism.** Rules 6 and 7 are Tapscript-scoped. The classic inscription
envelope is therefore still legal verbatim in SegWit v0, so long as its pushes
respect rule 2. Rabinow's `JXL-n-hide` uses this layout:

```
OP_1 OP_NOTIF  [six 255-byte PUSHDATA1 pushes]  OP_ENDIF OP_1
0x51 0x64      (0x4c 0xff <255 B>) x6           0x68 0x51
```

`OP_NOTIF` is not entered because `OP_1` is true; the trailing `OP_1` makes the
P2WSH spend succeed. Data rides in the six pushes.

**Why it complies.** `evaluate_p2wsh` applies only rule 2, and only to the
leading witness arguments plus pushes decoded inside the witnessScript. The
255-byte pushes pass. The `OP_NOTIF` is never examined because the P2WSH path
never reaches the Tapscript conditional check. Outputs are standard P2WSH at
34 bytes (rule 1).

**Capacity.** The `JXL-n-hide` codec caps each witnessScript at **1,650 bytes**,
which its error strings call "Knots' 1,650-byte policy target", and spreads the
payload across many commit outputs revealed by one reveal transaction.
`bip110-packer`'s equivalent `p2wsh-envelope` channel reports about
**9.9 KB/input**. Core's own standardness ceiling for a P2WSH script is 3,600
bytes.

**Detection signals.**
- Witness v0 spends whose witnessScript is a run of maximal (255-byte)
  `OP_PUSHDATA1` pushes inside a never-taken `OP_IF`/`OP_NOTIF` branch,
  terminating `OP_ENDIF OP_1`.
- The literal prefix `51 64` and suffix `68 51` for the `JXL-n-hide` variant.
- Many sibling P2WSH commit outputs of equal value spent together in one
  reveal.
- General shape: a P2WSH witnessScript containing pushes that no subsequent
  opcode consumes.

**False positives.** Low. Legitimate P2WSH scripts (multisig, HTLCs, vaults)
push keys and hashes at 33 or 32 bytes and then use them.

**Provenance.**
- **Steve Rabinow**, "JXL-n-hide", announced on his timeline 2026-07-24. The
  announcement lists the properties "No Inscriptions-style decoding / Doesn't
  use Taproot / Knots policy standard / Cheaper than Inscriptions (fee/quality)
  / ~4x cheaper than OP_RETURN", and credits knotslies.com as inspiration.
  Gist link post: <https://x.com/steverabinow/status/2080670985234088071>
- Implementation gist `jxl_codec_core.py` (47 KB), created 2026-07-24:
  <https://gist.github.com/stevenrabinow-hash/2da112b50cf76ba041d2d87c71be2f6a>
  The layout constants quoted above (`SCRIPT_PREFIX`, `PUSH_OPCODE`,
  `SCRIPT_SUFFIX`, `MAX_PUSHES_PER_SCRIPT`, the 1,650-byte cap) are read
  directly from that file.
- `bip110-packer` `p2wsh-envelope` channel, described as "the classic envelope,
  `OP_IF` is legal in v0".

<a id="w4"></a>
#### W4. Witness-argument drop channel

**Mechanism.** Instead of embedding data in the script, place it in ordinary
witness **stack items** that a tiny script immediately drops. Keeps the script
small and the shape simple.

**Why it complies.** Each witness stack item must respect rule 2's 256-byte cap
(these are script arguments, not the exempt script blob), so items are chunked
at 255 bytes or less.

**Capacity.** About **255 KB/input** per `bip110-packer`.

**Detection signals.** A witness with a large number of similarly-sized
(around 255-byte) stack items relative to a very short script, where the script
consists mostly of drops. Item-count and drop-count correlation is the
fingerprint. Medium detectability: legitimate large witnesses exist, but not
with this push-to-logic ratio.

**Provenance.** `bip110-packer` `witness-args` channel.

<a id="w5"></a>
#### W5. Control-block steganography

**Mechanism.** On a Taproot script-path spend the internal key is never signed
with, and the Merkle sibling hashes are unconstrained. Both are therefore free
bytes that still produce a valid Taproot commitment: about 31 bytes of internal
key plus up to 7 siblings at 32 bytes each.

**Why it complies.** Rule 5 constrains only the control block's total length
(257 bytes, depth 7). Nothing constrains its contents.

**Capacity.** About **224 B/input**.

**Detection signals.** Weak by construction. The tool's own README calls it
"indistinguishable from an ordinary deep script-tree spend". The only real
signal is circumstantial: an unusually deep declared Merkle path (close to the
depth-7 maximum) on a spend whose revealed script is trivial, repeated across
many inputs. Treat as a documented blind spot rather than a detection target.

**Provenance.** `bip110-packer` `control-block` channel, described there as the
most novel of the seven.

<a id="p1"></a>
#### P1. Polyglot overlay: making the raw transaction a valid file

**Mechanism.** This is not a separate location but a payload-shaping technique
layered on W1/W3/W4. The file is re-encoded so that the script framing bytes
land inside regions the file format ignores, making one **contiguous suffix of
the raw transaction** a valid file on its own. No push reassembly is needed:
`getrawtransaction ... | xxd -r -p | tail -c +N` yields the file.

The `JXL-n-hide` source states the method precisely for JPEG XL: the codestream
is split across `jxlp` container boxes, and small `free` boxes absorb the
Bitcoin witness-script framing. Container formats with skippable or padding
chunks (JXL, MP4, PDF, gzip/tar, WASM) all admit the same treatment.

**Why it matters for classification.** It changes the threat model from "an
indexer can reconstruct this" to "any file carver scanning block files will
extract it automatically", which is the basis of the abuse and legal-exposure
arguments being made publicly. Rabinow notes of the PDF demo that it "was
re-encoded from the original, but it's 100% valid and openable as a standard
PDF. A file carver would recognize it and extract it from the unobfuscated
block file."

**Detection signals.** Scan raw transaction bytes (not just decoded pushes) for
file magic at any offset: `%PDF`, `\x89PNG`, `GIF8`, `\xFF\xD8\xFF`,
`\x1F\x8B` (gzip), `\x00asm` (WASM), `ftyp` (ISO-BMFF/MP4), and the JXL
signature `0000000c 4a584c20 0d0a870a`. Because the payload is contiguous and
unobfuscated, a bounded magic-byte scan is cheap and has near-zero false
positives for the multi-byte signatures.

**Provenance.** **Steve Rabinow**'s "BIP110 ADVENT" series, which he expands as
**"Arbitrary Data Validly Embedded in Native Transactions"**. Each entry pairs
a claim with a live mainnet txid and a `tail -c` recovery command:

| Date | Entry | Payload | Recovery offset |
| --- | --- | --- | --- |
| 2026-07-24 | JXL-n-hide | Ultra-high-resolution JXL image | `tail -c +1073` |
| 2026-07-28 | ADVENT: PDFs | Bitcoin whitepaper | `tail -c +4517` |
| 2026-07-28 | ADVENT: Animated GIFs | GIF | `tail -c +154` |
| 2026-07-29 | BIPflix | Contiguous MP4 video | `tail -c +166` |
| 2026-07-30 | ADVENT: nerd edition | WASM quine executable | `tail -c +251` |
| 2026-08-07 | ADVENT: Endgame | `tar.gz` of entire directories, up to 400 kB | `tail -c +1073`, `gunzip` |

Series index post (Endgame, 2026-08-07):
<https://x.com/steverabinow/status/2085703735762698466>

The Endgame entry states the payload was a live malware sample. **Do not fetch
or decode that transaction.** It is cited here only as provenance for the
technique's existence and claimed capacity.

Jason Hughes ([@wk057](https://x.com/wk057)) independently characterised the
series on 2026-08-07: "Not only does it embed contiguous arbitrary data in a
BIP-110 compatible txn, it exploits a default policy delta where Knots is
*more* permissive than Core to get the txn mined by a Knots miner only." See
[open question 1](#q1).

### Family O: output-field carriers

Data in the hash or key field of an otherwise-standard output. Rule 1 is the
only rule that applies, and these fields are cryptographically opaque, so no
node can distinguish payload from a real hash. Carrier outputs are typically
unspendable and become permanent UTXO-set bloat.

Reference sizes, all passing rule 1: P2PKH 25 B, P2SH 23 B, P2WPKH 22 B,
P2WSH 34 B, P2TR 34 B. For contrast, and out of scope: P2PK is 35 B
(compressed) and bare 1-of-3 multisig about 105 B, both failing rule 1.

<a id="o1"></a>
#### O1. Fake P2TR output key

**Mechanism.** Any 32 bytes placed as a P2TR x-only output key in
`OP_1 <32 B>` (34 bytes). Nodes never check the key is a valid curve point
until someone tries to spend it.

**Capacity.** 32 B/output.

**Detection signals.**
- **`lift_x` failure.** About 50% of random 32-byte values are off-curve
  (x is valid only if `x^3 + 7` is a quadratic residue mod p, per BIP-340). An
  off-curve P2TR output is **provably unspendable**, and no wallet mints one.
  This is a near-zero-false-positive signal, and it is cheap.
- Sophisticated embedders grind each chunk on-curve at roughly 2x work and one
  lost bit per chunk, defeating the curve test. Fall back to multi-output
  reassembly plus never-spent dust heuristics.

**Relevant policy history.** The proposal to make off-curve P2TR outputs
non-standard,
[bitcoin/bitcoin#24106](https://github.com/bitcoin/bitcoin/pull/24106), was
**closed unmerged** in January 2022 over batch-withdrawal concerns, so these
outputs still relay on both Core and Knots today.

**Provenance.** `bip110-packer` `fake-key` channel (which flags it as "UTXO-set
bloat, worst case"). Antoine Poinsot cited users storing data on unspendable
Taproot outputs during the 2025 datacarrier debate.

<a id="o2"></a>
#### O2. Fake P2WSH: OLGA, Bitcoin Stamps, SRC-20

**Mechanism.** File bytes packed directly into the 32-byte witness-script-hash
field of P2WSH outputs. A 2-byte big-endian length prefix, then the blob split
into 32-byte chunks with the final chunk zero-padded, each chunk becoming one
`OP_0 <32 B>` output (34 bytes).

**This is a live protocol that has already migrated onto the compliant path.**
Bitcoin Stamps moved from 1-of-3 bare multisig (about 105-byte outputs, failing
rule 1, and blocked by `permitbaremultisig=false`) to OLGA precisely because
P2WSH accepts arbitrary 32-byte values at exactly the rule-1 limit. RDTS is the
kind of policy that motivated the migration, and the migrated form passes it.

**Capacity.** 32 B/output; with the standard 198-output ceiling,
`198 x 32 - 2` = **6,334 bytes/tx**.

**Detection signals.** Strong and structural:
- A run of two or more consecutive equal-value dust P2WSH outputs.
- The first carrier's leading two bytes decode to a length consistent with
  `32 x N - 2`.
- The reassembled stream carries a file magic or a known header.
- Outputs are provably unspendable (no preimage can exist) and stay unspent.

**False positives.** Low. Ordinary payments rarely emit many sequential
identical-value P2WSH outputs.

**Provenance.** JP Janssen, "File Storage in P2WSH Outputs" (OLGA), valid from
block 833,000.
- <https://github.com/mikeinspace/stamps/blob/main/OLGA.md>
- <https://github.com/mikeinspace/stamps/blob/main/BitcoinStamps.md>
- <https://stampchain.io/faq>

Note `docs/classification.md` already records OLGA as a known exclusion.

<a id="o3"></a>
#### O3. Fake P2PKH / P2SH / P2WPKH

**Mechanism.** 20 arbitrary bytes in the `hash160` field of a standard output.
P2PKH `OP_DUP OP_HASH160 <20 B> OP_EQUALVERIFY OP_CHECKSIG` (25 B), P2SH
`OP_HASH160 <20 B> OP_EQUAL` (23 B), P2WPKH `OP_0 <20 B>` (22 B).

**Capacity.** 20 B/output, chainable to roughly 58 KB/tx within the standard
transaction size limit.

**Detection signals.** Concatenate hash fields of consecutive same-type outputs
and test for file magic, long UTF-8 runs, or a length header. Entropy is a
weaker secondary signal: plaintext payloads sit below the near-uniform entropy
of a genuine hash, but compressed or encrypted payloads defeat it entirely. A
single such output is indistinguishable from a real payment; only multi-output
structure carries the signal.

**Provenance.** Matzutt et al., "A Quantitative Analysis of the Impact of
Arbitrary Blockchain Content on Bitcoin", FC 2018,
<https://fc18.ifca.ai/preproceedings/6.pdf>. Sward, Vecna and Stonedahl,
"Data Insertion in Bitcoin's Blockchain", Ledger 3 (2018),
<https://ledgerjournal.org/ojs/ledger/article/download/101/93/613>.
Deployed historically by CryptoGraffiti, Apertus and Satoshi Uploader.

<a id="o4"></a>
#### O4. OP_RETURN (the sanctioned channel)

Retained Knots policy allows one output with an 80-byte payload in an 83-byte
script. Already fully covered by the existing `data_protocols` lens. Listed
only for completeness and because it is the cost baseline the other techniques
measure themselves against (W1 and W3 are described by their authors as
roughly 4x cheaper per byte, thanks to the witness discount).

### Family F, S, C: low-capacity and undetectable channels

<a id="f1"></a>
#### F1. Transaction-field steganography

RDTS constrains outputs and pushed data, not `nVersion`, `nLockTime`,
`nSequence`, amounts, counts or ordering.

- `nSequence`: about 31 bits/input theoretically, but only 1 to 2 bits while
  still looking normal, since wallets use a few canonical values. Per-input
  `nSequence` entropy is the cleanest single tell.
- Output amounts: low-order satoshi bits, roughly 10 to 24 bits per amount
  while looking natural. Signal is a uniform low-order-digit distribution
  across many outputs.
- `nLockTime`: a nonzero locktime combined with all-final input sequences is
  self-contradictory and is the classic tell.
- Input/output ordering: `log2(i!) + log2(o!)` bits, detectable only as
  "matches no known wallet ordering algorithm", with BIP-69 as the reference.

`bip110-packer` implements the `nSequence`-plus-amount variant as its `stego`
channel at about **2 B/output**. Useful as ensemble features, not as a primary
lens. False-positive risk is high in isolation.

<a id="s1"></a>
#### S1. Signature and nonce channels

Grinding ECDSA `r` or Schnorr `R.x` yields roughly 4 bytes per signature at
2^32 work. Simmons-style broadband subliminal channels carry 32 bytes per
signature with no grinding but require a colluding receiver holding the key.

Detection is statistical (nonce bias, per Breitner and Heninger, "Biased Nonce
Sense", <https://eprint.iacr.org/2019/023.pdf>) and needs many signatures from
one signer, which a current-state mempool viewer does not have. Note also that
benign 1-bit low-`r` grinding is ubiquitous in Bitcoin Core wallets and must be
excluded before any claim is made. **Recommend documenting as out of reach
rather than implementing.**

<a id="c1"></a>
#### C1. Key-path commitments (pay-to-contract, sign-to-contract)

`Q = P + H(P||c)G` commits to arbitrary data `c` in a Taproot output key; the
data itself stays off-chain. Used legitimately by OpenTimestamps, RGB, Taproot
Assets and LNPBP single-use seals.

Bit-for-bit identical to a normal Taproot payment, therefore **undetectable in
principle**. Zero bulk data is stored on-chain, so it is also low-value to a
data-carriage classifier. Document as a known limit.

---

## 5. Suggested classifier mapping

A concrete shape for downstream design. Not a decision.

The catalog's own extension rule (`docs/classification.md`, "Known exclusions
and extension boundary") says a new question belongs in a **new versioned
classifier**, not folded into an existing lens. Two of the three options below
follow that; the third is a genuine bug fix to an existing lens.

**A. Fix `data_protocols` for the compliant `ord` envelope (in-place, v3).**
Teach `parse_ord_envelope` the bare-`ord` + push-drop grammar so W1 receives
the existing `inscription` and `brc20` labels. This is not a new question, it
is the same question with an updated wire format, so it belongs in the existing
lens with a version bump. Mirrors ord#4545's own parser structure, including
the stack-depth counter and the 256-byte push rejection.

**B. New lens `data_carriage_shape` v1 (heuristic, multi-label).**
Answers "does this transaction have the *shape* of a bulk carrier,
independently of protocol branding?". Candidate labels:

| Label | Fires on |
| --- | --- |
| `push_drop_witness` | W1, W3, W4: balanced push/drop runs in a revealed script |
| `opcode_value_coding` | W2: OP_PLENTY magic or push-free alphabet run |
| `p2wsh_envelope` | W3: never-taken `OP_IF` branch of maximal pushes in v0 |
| `output_key_carrier` | O1, O2, O3: multi-output hash/key reassembly |
| `off_curve_p2tr` | O1: `lift_x` failure, provably unspendable |
| `embedded_file_magic` | P1: file signature found in raw transaction bytes |

Required facts: raw transaction (already fetched for classification) and, for
the input-side labels, spent-output scripts. Everything needed is already
resolved for the existing lenses, so no new RPC surface is required. That
matters: it means this lens costs no extra node round-trips.

Implementation status: A shipped as `data_protocols` version 3. B shipped in
five stages and is now `data_carriage_shape` version 5. The implemented B
labels are `push_drop_witness`, `opcode_value_coding`, `p2wsh_envelope`,
`witness_argument_carrier`, `output_key_carrier`, `off_curve_p2tr`, and
`embedded_file_magic`. The P2WSH envelope recognizes only the exact six-push
JXL-n-hide grammar, the witness-argument label requires exact 255-byte
item-to-drop correlation, and the output carrier recognizes only the
self-consistent OLGA P2WSH grammar, not arbitrary O1 or O3 reassembly. The file
signature label uses a constant-memory consensus-serialization scan and
deliberately excludes short collision-prone prefixes.

**C. Extend `structure` with a carried-bytes fact. Shipped.**
`recognized_carried_bytes` retains `op_return_bytes` and adds only byte counts
backed by the implemented high-confidence carrier shapes. Embedded file magic
adds zero because it can overlap another carrier. Evidence truncation does not
truncate the metric. The public distribution uses this fact on a 512 KiB axis,
with exact reference ticks for 255-byte items, the 1,020-byte witness minimum,
the 1,530-byte JXL-n-hide envelope, and the 6,334-byte OLGA maximum.

### Suggested priority

1. **A**, the `ord` envelope fix. Smallest change, closes a real gap on a
   technique with a merged-and-waiting parser in the reference indexer, and it
   reuses the existing payload parser.
2. **`push_drop_witness`** plus **`opcode_value_coding`**. These cover the
   highest-capacity techniques (W1, W2, W3, W4) with mechanical,
   low-false-positive byte signals.
3. **`output_key_carrier`** plus **`off_curve_p2tr`**. Closes the
   already-documented OLGA gap and adds a cheap, near-zero-false-positive
   unspendability test.
4. **`embedded_file_magic`**. Cheap bounded scan, high explanatory value for
   users, directly addresses the file-carving concern.
5. **C**, the carried-bytes fact, shipped after the carrier definitions above.
6. Field, signature and commitment channels (F1, S1, C1): document as limits.
   Do not implement.

### Honesty constraints to carry over

The existing catalog's discipline applies to every label above.

- These are **fingerprints and heuristics**, never proof of intent. A
  push-drop run is a shape, not an accusation.
- Absence of a label is not evidence of absence, particularly for on-curve
  ground P2TR keys and for encrypted payloads.
- Detection must stay independent of the `knots_bip110` verdict. The entire
  point is that these transactions are legitimately `compatible`. A carrier
  label must never be presented as a policy violation, and the policy lens must
  not inherit carriage labels.
- Several techniques are demonstrations by policy critics rather than deployed
  protocols. Prevalence claims need on-chain measurement, not source counts.

---

## 6. Open questions

<a id="q1"></a>
**1. What exactly is the Knots-versus-Core policy delta being exploited?**
Rabinow claims his ADVENT transactions are "Knots standard (exclusively!)", and
Jason Hughes describes "a default policy delta where Knots is *more* permissive
than Core". Pulling the other direction, `bip110-packer`'s own test matrix
records the *opposite* result for its bulk tapleaf channel: `testmempoolaccept`
returns `allowed=true` on Core v30 but **`allowed=false: bad-witness-witness-size`
on Knots v29.3**, while both nodes mine it. The `JXL-n-hide` codec's
1,650-byte cap on the serialized witness stack, which its error strings
attribute to Knots policy, looks like a deliberate accommodation of that same
Knots limit.

A plausible reading is that Knots applies a witness-size relay limit Core does
not (pushing bulk carriers below it), while separately relaxing some
per-item standardness check that Core still enforces, for example Core's
`MAX_STANDARD_P2WSH_STACK_ITEM_SIZE` of 80 bytes against RDTS rule 2's 256.
**This is inference, not verified.** It should be checked against Knots source
before any of it is written into user-facing text.

This matters directly for Atlas: it determines whether these carriers are
observable in a Knots-sourced mempool at all, or only ever appear in mined
blocks. A technique that Knots will mine but not relay is invisible to a
current-state mempool viewer polling a Knots node, and the comparison view
across a Core source and a Knots source would show exactly that asymmetry.

**2. Prevalence.** Every technique here is sourced from its author or tool.
None of the capacity claims is a measurement of deployed usage. Before
prioritising, measure how many current mempool transactions actually match each
signal.

**3. Is ord#4545 the format that ships?** It is open and explicitly gated on
BIP-110 activating. If RDTS does not activate, the compliant envelope may never
see broad use, though `bip110-packer` and OP_PLENTY are independent of it.

**4. Deployment status.** As of 2026-08-08 the RDTS enforcement PR
([bitcoinknots/bitcoin#238](https://github.com/bitcoinknots/bitcoin/pull/238))
is **closed and unmerged**; the runnable enforcement client is the author's own
fork. Reported miner signaling has been far below the 55% threshold. Some
secondary coverage incorrectly states BIP-110 already ships in Knots. Treat
activation as unresolved.

---

## 7. Source index

**Primary implementations**

| Source | Author | URL |
| --- | --- | --- |
| ord BIP-110 envelope parser (open) | lifofifoX | <https://github.com/ordinals/ord/pull/4545> |
| ordpool-parser BIP-110 support (open) | hans-crypto | <https://github.com/ordpool-space/ordpool-parser/pull/22> |
| `bip110-packer`, 7 channels, MIT | Richard J. Safier / Monumental Systems | <https://github.com/MonumentalSystems/bip110-packer> |
| OP_PLENTY codec | Steve Rabinow | <https://gist.github.com/stevenrabinow-hash/b71d7e085cb67a91b4553f750a1086dd> |
| JXL-n-hide codec | Steve Rabinow | <https://gist.github.com/stevenrabinow-hash/2da112b50cf76ba041d2d87c71be2f6a> |
| OLGA / Bitcoin Stamps P2WSH format | JP Janssen | <https://github.com/mikeinspace/stamps/blob/main/OLGA.md> |

**Specification and policy**

| Source | URL |
| --- | --- |
| BIP-110 text | <https://github.com/bitcoin/bips/blob/master/bip-0110.mediawiki> |
| BIP-110 spec PR (merged 2026-02-07) | <https://github.com/bitcoin/bips/pull/2017> |
| Knots enforcement PR (closed, unmerged) | <https://github.com/bitcoinknots/bitcoin/pull/238> |
| Bitcoin Core v30.0 release notes | <https://bitcoincore.org/en/releases/30.0/> |
| Core: uncap datacarrier by default | <https://github.com/bitcoin/bitcoin/pull/32406> |
| Core: allow >1 OP_RETURN per tx | <https://github.com/bitcoin/bitcoin/pull/32381> |
| Core: off-curve P2TR non-standard (closed) | <https://github.com/bitcoin/bitcoin/pull/24106> |
| BIP-340 (`lift_x`) | <https://github.com/bitcoin/bips/blob/master/bip-0340.mediawiki> |

**Announcements and commentary**

| Source | Date | URL |
| --- | --- | --- |
| lifofifo, "Ordinals are now BIP-110 ready" | 2026-07-03 | <https://x.com/lifofifo/status/2072828299097555000> |
| Rabinow, OP_PLENTY | 2026-07-22 | <https://x.com/steverabinow/status/2079613801976967450> |
| Rabinow, ADVENT: Endgame | 2026-08-07 | <https://x.com/steverabinow/status/2085703735762698466> |
| Peter Todd compliant proof-of-concept | 2025-10-27 | <https://groups.google.com/g/bitcoindev/c/nOZim6FbuF8> |
| Jameson Lopp, "A Layman's Guide to BIP-110" | 2026-02-23 | <https://blog.lopp.net/a-laymans-guide-to-bip-110/> |

**Academic**

- Matzutt et al., "A Quantitative Analysis of the Impact of Arbitrary
  Blockchain Content on Bitcoin", FC 2018,
  <https://fc18.ifca.ai/preproceedings/6.pdf>
- Sward, Vecna, Stonedahl, "Data Insertion in Bitcoin's Blockchain",
  Ledger 3 (2018),
  <https://ledgerjournal.org/ojs/ledger/article/download/101/93/613>
- Bartoletti and Pompianu, "An Analysis of Bitcoin OP_RETURN Metadata",
  FC Workshops 2017, <https://arxiv.org/abs/1702.01024>
- Breitner and Heninger, "Biased Nonce Sense", <https://eprint.iacr.org/2019/023.pdf>
- Partala, "Provably Secure Covert Communication on Blockchain" (BLOCCE),
  Cryptography 2018, <https://www.mdpi.com/2410-387X/2/3/18>
