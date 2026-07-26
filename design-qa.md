# Exact Rule-Combination Terrain Design QA

## Visual source

- Reference:
  `/Users/anthonymilton/.codex/generated_images/019f878f-7918-7cc2-b3d8-ba7fba500b3f/call_04dcpqDJcskM8WEEMwYV1pn6.png`
- Reference state: R2 + R7 exact bucket selected, count mode active.
- Comparison viewport: 1586 × 992.
- Final implementation:
  `/Users/anthonymilton/.codex/visualizations/2026/07/22/019f878f-7918-7cc2-b3d8-ba7fba500b3f/mempool-atlas-combinations/implementation-final.png`
- Final side-by-side comparison:
  `/Users/anthonymilton/.codex/visualizations/2026/07/22/019f878f-7918-7cc2-b3d8-ba7fba500b3f/mempool-atlas-combinations/reference-vs-final.png`
- Responsive captures:
  `/Users/anthonymilton/.codex/visualizations/2026/07/22/019f878f-7918-7cc2-b3d8-ba7fba500b3f/mempool-atlas-combinations/responsive-820x900.png`
  and
  `/Users/anthonymilton/.codex/visualizations/2026/07/22/019f878f-7918-7cc2-b3d8-ba7fba500b3f/mempool-atlas-combinations/responsive-390x844.png`.

## Comparison history

1. The first real-data comparison established the canonical exact-bucket
   grouping and the calm shared blue foundation for R7-only and R2 + R7. It
   also exposed a redundant primary-rule interaction model and incomplete
   status-bucket selection.
2. The implementation separated marginal rule filters from exact and partial
   bucket selection. An independent review then found three long-term issues:
   compatible detail used the wrong first-rejection label, selection rebuilt
   all geometry, and fixed label spacing could collapse a rare bucket's glyph
   area.
3. The final pass added selectable status regions, status-aware rejection copy,
   cached selection-only paints, key-indexed glyph painting, and adaptive
   layout spacing. The final comparison uses the same R2 + R7 selected state
   and viewport as the visual reference.

## Final visual assessment

- No P0, P1, or P2 visual issues remain.
- Every current transaction appears once in a compatible, indeterminate,
  unclassified, exact violating, or partial violating bucket.
- R7-only and R2 + R7 are separate but visually related blue buckets. The
  `+R2` marker makes their relationship explicit without a severe striped or
  contrasting carve-out.
- R1-only remains visibly separate in its rule colour. Partial buckets use an
  amber unresolved marker and cannot be mistaken for exact combinations.
- The inspector clearly distinguishes a marginal rule filter, an exact rule
  bucket, an incomplete rule bucket, and a status bucket.
- Sample rows show every proven and unresolved rule through compact labelled
  chips, while transaction detail keeps first rejection as secondary metadata.

## Interaction checks

- Selecting the R2 + R7 bucket scoped the inspector and sample table to the 75
  exact matches in the observed snapshot, with no rule filter left pressed.
- Selecting R2 returned all 75 proven R2 matches and showed R2 + R7 chips in
  the sample table.
- Selecting R7 returned 289 matches and highlighted both `exact:40` and
  `exact:42`, proving that marginal rule totals overlap.
- Selecting Compatible changed the inspector to a status population and showed
  `First rejection: None` for its loaded sample.
- Selecting `partial:01:7e` showed two proven R1 transactions with R2 through
  R7 unresolved and explicitly said the bucket was not an exact rule set.
- ArrowRight moved focus and selection from R7-only to R2 + R7 after redraw.
- Count and vsize controls changed their pressed state and Canvas text
  alternative; Count was restored for the final comparison.
- The fee-rate-by-age lens opened with all 25,056 current transactions and
  returned to the selected rule-combination terrain.

## Responsive and accessibility checks

- 820 × 900: body and document widths remained 820 with no horizontal
  overflow; all seven observed region controls remained present.
- 390 × 844: body and document widths remained 390 with no horizontal
  overflow; every observed region control retained positive width and height.
- Compatible, indeterminate, unclassified, exact, and partial terrain headers
  are keyboard-selectable buttons with descriptive accessible labels.
- The Canvas has a current text alternative, and the DOM controls provide a
  complete keyboard route to every observed region.

## Diagnostics

- Browser console warnings and errors: none.
- `just format`: passed.
- `just lint`: passed.
- `just test`: passed, including 66 frontend tests.
- `just build`: passed.

final result: passed
