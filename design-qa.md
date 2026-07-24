# Classification Terrain Design QA

## Visual source

- Reference:
  `/Users/anthonymilton/.codex/generated_images/019f878f-7918-7cc2-b3d8-ba7fba500b3f/call_aEuGgUvatsfpDaRWWXrxNNOs.png`
- Reference state: Classification terrain, Rule 2 selected, vsize active.
- Comparison viewport: 1487 × 1058 at device scale factor 1.
- Final full comparison:
  `/tmp/mempool-atlas-design-comparison-pass3.png`
- Final terrain comparison:
  `/tmp/mempool-atlas-terrain-focus-comparison-pass3.png`
- Final inspector comparison:
  `/tmp/mempool-atlas-inspector-focus-comparison-pass3.png`

## Comparison history

1. The first comparison used different selected-rule and size-metric states, so
   it was not accepted as a valid match.
2. The second comparison normalized both views to Rule 2 and vsize. It exposed
   a P2 hierarchy issue: raw proportional region sizing compressed low-count
   rule territories until their labels and transaction structure were hard to
   inspect.
3. The implementation retained exact transaction and vsize totals but changed
   only the territory-frame layout to square-root weighting. Transaction tiles
   still use the selected count or vsize metric. The third comparison shows all
   seven exact rule territories, the unresolved-primary population, and the
   non-classified population as legible, distinct regions.

## Final visual assessment

- No P0, P1, or P2 visual issues remain.
- The dark grid, cyan identity, compact node header, lens controls, coverage
  legend, dominant terrain, and persistent rule inspector follow the selected
  reference.
- Rule 2 is visibly selected in both the terrain and inspector.
- Compatible, indeterminate, violating, unresolved-primary, and not-classified
  states remain visually distinct without implying rejection or consensus
  invalidity.
- Exact counts and virtual sizes remain visible as labels while the terrain
  preserves enough area for every rule to be inspected.
- The selected transaction panel exposes the complete seven-rule result and
  bounded evidence returned by the API.

## Interaction checks

- Lens tabs support click, ArrowLeft, and ArrowRight navigation with a single
  active tab.
- Rule controls support selection, ArrowLeft, ArrowRight, Home, and End.
- End selects the unresolved-primary population and exposes its sample list.
  ArrowLeft then selects Rule 7, confirming focus remains usable after redraw.
- Count and vsize controls update their pressed state and the transaction tile
  metric.
- Selecting a sample transaction updates the exact txid and its rule detail.
- A present but unclassified transaction returns HTTP 503 with
  `transaction is present but not yet classified`; the UI does not invent
  evidence.
- The fee-rate-by-age secondary lens remains reachable and returns to the
  classification terrain through keyboard navigation.

## Responsive and accessibility checks

- 820 × 900: document width 820, body width 820, no horizontal overflow.
- 390 × 844: document width 390, body width 390, no horizontal overflow.
- The Canvas has a readable text alternative and DOM rule controls.
- The mobile header, lens controls, coverage summary, status totals, and terrain
  remain readable at the narrow breakpoint.

## Diagnostics

- Browser console warnings and errors: none.
- `just format`: passed.
- `just lint`: passed.
- `just test`: passed, including 55 frontend tests.
- `just build`: passed.

final result: passed
