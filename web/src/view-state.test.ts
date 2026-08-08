import { describe, expect, it } from "vitest";

import type { ComparisonPolicyFilter } from "./comparison-model";
import {
  mergeCampaignQuery,
  parseComparisonPolicyFilter,
  parseComparisonViewState,
  parseNodeViewState,
  serializeComparisonPolicyFilter,
  serializeComparisonViewState,
  serializeNodeViewState,
} from "./view-state";

const TXID = "ab".repeat(32);

describe("campaign URL state", () => {
  it("preserves repeated UTM parameters without retaining unrelated input", () => {
    expect(
      mergeCampaignQuery(
        "source=core",
        "?utm_source=social&utm_source=nostr&utm_campaign=launch&ignored=value",
      ),
    ).toBe(
      "source=core&utm_source=social&utm_source=nostr&utm_campaign=launch",
    );
  });

  it("keeps serialized application state authoritative", () => {
    expect(
      mergeCampaignQuery(
        "left=core&right=knots&region=common",
        "?left=ignored&utm_medium=profile",
      ),
    ).toBe("left=core&right=knots&region=common&utm_medium=profile");
  });
});

describe("node URL state", () => {
  it("parses source, classifier, rule, and transaction state", () => {
    expect(
      parseNodeViewState(
        `?source=core&classifier=data_protocols&label=runes&label=inscription&match=all&rule=element_size&txid=${TXID}&ignored=value`,
      ),
    ).toEqual({
      source: "core",
      classifier: "data_protocols",
      classifierLabels: ["inscription", "runes"],
      classifierMatch: "all",
      selection: { kind: "rule", rule: "element_size" },
      txid: TXID,
    });
  });

  it("parses status and canonical violation regions", () => {
    expect(parseNodeViewState("?region=unclassified").selection).toEqual({
      kind: "region",
      regionKey: "unclassified",
    });
    expect(parseNodeViewState("?region=partial:2:40").selection).toEqual({
      kind: "region",
      regionKey: "partial:02:40",
    });
  });

  it("drops a conflicting rule and region without disturbing the txid", () => {
    expect(
      parseNodeViewState(`?rule=element_size&region=exact:02&txid=${TXID}`),
    ).toEqual({
      source: null,
      classifier: null,
      classifierLabels: [],
      classifierMatch: "any",
      selection: null,
      txid: TXID,
    });
  });

  it("drops malformed, duplicated, and out-of-range values", () => {
    expect(
      parseNodeViewState(
        "?source=..&source=core&rule=not-a-rule&region=exact:80&txid=1234",
      ),
    ).toEqual({
      source: null,
      classifier: null,
      classifierLabels: [],
      classifierMatch: "any",
      selection: null,
      txid: null,
    });
  });

  it("accepts uppercase hex txids and serializes canonical state in order", () => {
    const parsed = parseNodeViewState(
      `?txid=${TXID.toUpperCase()}&region=EXACT:02&source=core`,
    );
    expect(parsed.txid).toBe(TXID);
    expect(parsed.selection).toBeNull();

    expect(
      serializeNodeViewState({
        source: "core",
        classifier: "data_protocols",
        classifierLabels: ["runes", "inscription", "runes"],
        classifierMatch: "all",
        selection: { kind: "region", regionKey: "exact:2" },
        txid: TXID.toUpperCase(),
      }),
    ).toBe(
      `source=core&classifier=data_protocols&label=inscription&label=runes&match=all&region=exact%3A02&txid=${TXID}`,
    );
  });

  it("omits structurally invalid values while serializing", () => {
    expect(
      serializeNodeViewState({
        source: "..",
        classifier: "Not Valid",
        classifierLabels: ["invalid-label", "valid_label"],
        classifierMatch: "all",
        selection: {
          kind: "region",
          regionKey: "partial:00:40",
        },
        txid: "z".repeat(64),
      }),
    ).toBe("label=valid_label&match=all");
  });

  it("canonicalizes classifier label queries and defaults unknown modes to any", () => {
    expect(
      parseNodeViewState(
        "?label=p2wsh&label=bad-label&label=p2tr&label=p2wsh&match=neither",
      ),
    ).toMatchObject({
      classifierLabels: ["p2tr", "p2wsh"],
      classifierMatch: "any",
    });
  });
});

describe("comparison policy filter URL state", () => {
  it.each<[string | null, ComparisonPolicyFilter]>([
    [null, { kind: "all" }],
    ["all", { kind: "all" }],
    ["rule:tapscript_op_if", { kind: "rule", rule: "tapscript_op_if" }],
    ["status:compatible", { kind: "status", status: "compatible" }],
    ["status:violating", { kind: "status", status: "violating" }],
    [
      "signature:partial:2:40",
      { kind: "signature", signature: "partial:02:40" },
    ],
  ])("parses %s", (value, expected) => {
    expect(parseComparisonPolicyFilter(value)).toEqual(expected);
  });

  it("uses all for unknown filters and omits it canonically", () => {
    expect(parseComparisonPolicyFilter("signature:exact:80")).toEqual({
      kind: "all",
    });
    expect(serializeComparisonPolicyFilter({ kind: "all" })).toBeNull();
  });
});

describe("comparison URL state", () => {
  it("parses a complete deep link", () => {
    expect(
      parseComparisonViewState(
        `?left=core&right=knots&region=left_only&side=left&filter=signature:exact:42&txid=${TXID}`,
      ),
    ).toEqual({
      left: "core",
      right: "knots",
      region: "left_only",
      side: "left",
      filter: { kind: "signature", signature: "exact:42" },
      txid: TXID,
    });
  });

  it("drops incomplete, equal, and duplicated source pairs", () => {
    expect(parseComparisonViewState("?left=core")).toMatchObject({
      left: null,
      right: null,
    });
    expect(parseComparisonViewState("?left=core&right=core")).toMatchObject({
      left: null,
      right: null,
    });
    expect(
      parseComparisonViewState("?left=core&left=knots&right=libre"),
    ).toMatchObject({ left: null, right: null });
  });

  it("drops invalid independent fields", () => {
    expect(
      parseComparisonViewState(
        "?left=core&right=knots&region=neither&side=middle&filter=status:violation&txid=nope",
      ),
    ).toEqual({
      left: "core",
      right: "knots",
      region: null,
      side: null,
      filter: { kind: "all" },
      txid: null,
    });
  });

  it("round-trips the aggregate violating filter without widening node regions", () => {
    const query =
      "left=core&right=knots&region=common&side=right&filter=status%3Aviolating";
    expect(serializeComparisonViewState(parseComparisonViewState(query))).toBe(
      query,
    );
    expect(parseNodeViewState("?region=violating").selection).toBeNull();
  });

  it("serializes a canonical query and omits the default filter", () => {
    expect(
      serializeComparisonViewState({
        left: "core",
        right: "knots",
        region: "common",
        side: "right",
        filter: {
          kind: "signature",
          signature: "partial:2:40",
        },
        txid: TXID.toUpperCase(),
      }),
    ).toBe(
      `left=core&right=knots&region=common&side=right&filter=signature%3Apartial%3A02%3A40&txid=${TXID}`,
    );

    expect(
      serializeComparisonViewState({
        left: "core",
        right: "knots",
        region: null,
        side: null,
        filter: { kind: "all" },
        txid: null,
      }),
    ).toBe("left=core&right=knots");
  });

  it("serializes a source pair atomically", () => {
    expect(
      serializeComparisonViewState({
        left: "core",
        right: "core",
        region: "left_only",
        side: "left",
        filter: { kind: "rule", rule: "element_size" },
        txid: null,
      }),
    ).toBe("region=left_only&side=left&filter=rule%3Aelement_size");
  });
});
