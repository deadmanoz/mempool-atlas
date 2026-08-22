import { afterEach, describe, expect, it, vi } from "vitest";

import { ComparisonDetailScheduler } from "./comparison-detail-scheduler";

describe("ComparisonDetailScheduler", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("coalesces rapid cursor moves into one detail request", () => {
    vi.useFakeTimers();
    const requestDetail = vi.fn();
    const scheduler = new ComparisonDetailScheduler(requestDetail, 300);

    for (let index = 1; index <= 30; index += 1) {
      scheduler.schedule(`tx-${index}`);
      if (index < 30) {
        vi.advanceTimersByTime(10);
      }
    }

    expect(requestDetail).not.toHaveBeenCalled();
    vi.advanceTimersByTime(300);

    expect(requestDetail).toHaveBeenCalledTimes(1);
    expect(requestDetail).toHaveBeenCalledWith("tx-30");
  });

  it("loads an unchanged direct selection immediately", () => {
    vi.useFakeTimers();
    const requestDetail = vi.fn();
    const scheduler = new ComparisonDetailScheduler(requestDetail, 300);

    scheduler.schedule("selected-tx");
    scheduler.loadNow("selected-tx");
    vi.runAllTimers();

    expect(requestDetail).toHaveBeenCalledTimes(1);
    expect(requestDetail).toHaveBeenCalledWith("selected-tx");
  });
});
