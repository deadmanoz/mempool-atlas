// @vitest-environment happy-dom

import { beforeEach, describe, expect, it } from "vitest";

import { setMembershipControlsComplete } from "./membership-controls";

const ids = [
  "fee-age-tab",
  "classification-lens-select",
  "minimum-fee-rate",
  "maximum-age",
  "minimum-vsize",
  "reset-filters",
];

beforeEach(() => {
  document.body.innerHTML = `
    <button id="fee-age-tab"></button>
    <select id="classification-lens-select"></select>
    <input id="minimum-fee-rate">
    <select id="maximum-age"></select>
    <input id="minimum-vsize">
    <button id="reset-filters"></button>
  `;
});

describe("membership controls", () => {
  it("keeps every membership-only action disabled until the snapshot is complete", () => {
    setMembershipControlsComplete(false);
    expect(
      ids.map(
        (id) =>
          (
            document.getElementById(id) as
              HTMLButtonElement | HTMLInputElement | HTMLSelectElement
          ).disabled,
      ),
    ).toEqual(ids.map(() => true));

    setMembershipControlsComplete(true);
    expect(
      ids.map(
        (id) =>
          (
            document.getElementById(id) as
              HTMLButtonElement | HTMLInputElement | HTMLSelectElement
          ).disabled,
      ),
    ).toEqual(ids.map(() => false));
  });
});
