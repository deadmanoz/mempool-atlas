import { vi, type Mock } from "vitest";

export interface ControlledResizeObserver extends ResizeObserver {
  readonly observed: Element[];
  trigger(): void;
}

export interface DistributionViewTestHarness {
  readonly canvasContext: {
    setTransform: Mock;
    clearRect: Mock;
    fillRect: Mock;
    beginPath: Mock;
    rect: Mock;
    fill: Mock;
    fillStyle: string;
    globalAlpha: number;
  };
  readonly resizeObservers: ControlledResizeObserver[];
  readonly cancelledAnimationFrames: number[];
  pendingAnimationFrames(): number;
  flushAnimationFrames(): Promise<void>;
  cleanup(): void;
}

export const installDistributionViewTestHarness =
  (): DistributionViewTestHarness => {
    let nextFrameId = 1;
    const animationFrames = new Map<number, FrameRequestCallback>();
    const cancelledAnimationFrames: number[] = [];
    const resizeObservers: ControlledResizeObserver[] = [];
    const canvasContext = {
      setTransform: vi.fn(),
      clearRect: vi.fn(),
      fillRect: vi.fn(),
      beginPath: vi.fn(),
      rect: vi.fn(),
      fill: vi.fn(),
      fillStyle: "",
      globalAlpha: 1,
    };

    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      const frameId = nextFrameId;
      nextFrameId += 1;
      animationFrames.set(frameId, callback);
      return frameId;
    });
    vi.spyOn(window, "cancelAnimationFrame").mockImplementation((frameId) => {
      cancelledAnimationFrames.push(frameId);
      animationFrames.delete(frameId);
    });

    class TestResizeObserver implements ControlledResizeObserver {
      readonly observed: Element[] = [];

      constructor(private readonly callback: ResizeObserverCallback) {
        resizeObservers.push(this);
      }

      observe(target: Element): void {
        this.observed.push(target);
      }

      unobserve(target: Element): void {
        const index = this.observed.indexOf(target);
        if (index !== -1) {
          this.observed.splice(index, 1);
        }
      }

      disconnect(): void {
        this.observed.length = 0;
      }

      trigger(): void {
        this.callback([], this);
      }
    }

    vi.stubGlobal("ResizeObserver", TestResizeObserver);
    class TestIntersectionObserver {
      constructor(private readonly callback: IntersectionObserverCallback) {}

      observe(target: Element): void {
        this.callback(
          [{ isIntersecting: true, target } as IntersectionObserverEntry],
          this as unknown as IntersectionObserver,
        );
      }

      unobserve(): void {}
      disconnect(): void {}
      takeRecords(): IntersectionObserverEntry[] {
        return [];
      }
      readonly root = null;
      readonly rootMargin = "0px";
      readonly thresholds = [0];
    }
    vi.stubGlobal("IntersectionObserver", TestIntersectionObserver);
    vi.stubGlobal("scheduler", { yield: () => Promise.resolve() });
    vi.spyOn(HTMLCanvasElement.prototype, "getContext").mockImplementation(
      () => canvasContext as unknown as CanvasRenderingContext2D,
    );
    const clientWidthDescriptor = Object.getOwnPropertyDescriptor(
      HTMLCanvasElement.prototype,
      "clientWidth",
    );
    Object.defineProperty(HTMLCanvasElement.prototype, "clientWidth", {
      configurable: true,
      get: () => 480,
    });
    const pixelRatioDescriptor = Object.getOwnPropertyDescriptor(
      window,
      "devicePixelRatio",
    );
    Object.defineProperty(window, "devicePixelRatio", {
      configurable: true,
      value: 2,
    });

    return {
      canvasContext,
      resizeObservers,
      cancelledAnimationFrames,
      pendingAnimationFrames: () => animationFrames.size,
      flushAnimationFrames: async () => {
        const pending = [...animationFrames.values()];
        animationFrames.clear();
        for (const callback of pending) {
          callback(performance.now());
        }
        await Promise.resolve();
        await Promise.resolve();
      },
      cleanup: () => {
        document.body.replaceChildren();
        vi.restoreAllMocks();
        vi.unstubAllGlobals();
        if (clientWidthDescriptor === undefined) {
          delete (
            HTMLCanvasElement.prototype as unknown as {
              clientWidth?: number;
            }
          ).clientWidth;
        } else {
          Object.defineProperty(
            HTMLCanvasElement.prototype,
            "clientWidth",
            clientWidthDescriptor,
          );
        }
        if (pixelRatioDescriptor === undefined) {
          delete (window as unknown as { devicePixelRatio?: number })
            .devicePixelRatio;
        } else {
          Object.defineProperty(
            window,
            "devicePixelRatio",
            pixelRatioDescriptor,
          );
        }
      },
    };
  };
