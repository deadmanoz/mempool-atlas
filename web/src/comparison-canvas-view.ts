import {
  paintActiveComparisonTransaction,
  resolveComparisonGeometry,
  renderComparisonCanvasProgressively,
  type ComparisonGeometry,
  type ComparisonLayout,
} from "./comparison-layout";
import {
  MAX_RETAINED_CANVAS_PIXELS,
  prepareCanvasBacking,
} from "./canvas-backing";
import type {
  ComparisonPolicyFilter,
  ComparisonRegionKey,
  ComparisonSide,
  CurrentComparison,
} from "./comparison-model";

export interface PreparedComparisonCanvas {
  comparison: CurrentComparison;
  geometry: ComparisonGeometry;
}

export type ComparisonCanvasRenderStatus = "rendered" | "superseded";

/**
 * Settle the latest selection owned by one publication. Selection churn is
 * not publication churn: the view aborts obsolete paint and retains at most
 * one current render, while publication replacement terminates this loop.
 */
export const renderLatestComparisonCanvas = async (
  ownsPublication: () => boolean,
  selectedRegion: () => ComparisonRegionKey,
  render: () => Promise<ComparisonCanvasRenderStatus>,
): Promise<void> => {
  while (ownsPublication()) {
    const renderedRegion = selectedRegion();
    const status = await render();
    if (
      status === "rendered" &&
      ownsPublication() &&
      selectedRegion() === renderedRegion
    ) {
      return;
    }
  }
};

const currentCanvasMetrics = (canvas: HTMLCanvasElement) => {
  const bounds = canvas.getBoundingClientRect();
  return {
    width: Math.max(1, Math.round(bounds.width)),
    height: Math.max(1, Math.round(bounds.height)),
    pixelRatio: Math.max(1, window.devicePixelRatio || 1),
  };
};

const policyPaintKey = (
  side: ComparisonSide,
  filter: ComparisonPolicyFilter,
): string => {
  if (filter.kind === "all") return `${side}:all`;
  if (filter.kind === "status") return `${side}:status:${filter.status}`;
  if (filter.kind === "rule") return `${side}:rule:${filter.rule}`;
  return `${side}:signature:${filter.signature}`;
};

export class ComparisonCanvasView {
  private geometry: ComparisonGeometry | null = null;
  private controller: AbortController | null = null;
  private readonly baseCanvas = document.createElement("canvas");
  private baseComparison: CurrentComparison | null = null;
  private baseRegion: ComparisonRegionKey | null = null;
  private basePolicyPaintKey: string | null = null;
  private paintedTransactionId: string | null = null;
  private desiredTransactionId: string | null = null;
  private pendingBase: Promise<ComparisonCanvasRenderStatus> | null = null;
  private pendingComparison: CurrentComparison | null = null;
  private pendingRegion: ComparisonRegionKey | null = null;
  private pendingPolicyPaintKey: string | null = null;

  constructor(private readonly canvas: HTMLCanvasElement) {}

  get layout(): ComparisonLayout | null {
    return this.geometry?.layout ?? null;
  }

  private releaseBaseBacking(): void {
    this.baseCanvas.width = 0;
    this.baseCanvas.height = 0;
  }

  invalidate(): void {
    this.controller?.abort();
    this.controller = null;
    this.pendingBase = null;
    this.pendingComparison = null;
    this.pendingRegion = null;
    this.pendingPolicyPaintKey = null;
    this.geometry = null;
    this.baseComparison = null;
    this.baseRegion = null;
    this.basePolicyPaintKey = null;
    this.paintedTransactionId = null;
    this.desiredTransactionId = null;
    this.releaseBaseBacking();
    delete this.canvas.dataset.renderedRegion;
    delete this.canvas.dataset.renderedTransaction;
  }

  prepareCandidate(comparison: CurrentComparison): PreparedComparisonCanvas {
    const { width, height, pixelRatio } = currentCanvasMetrics(this.canvas);
    return {
      comparison,
      geometry: resolveComparisonGeometry(
        comparison,
        width,
        height,
        pixelRatio,
        null,
      ).geometry,
    };
  }

  canCommitCandidate(candidate: PreparedComparisonCanvas): boolean {
    const { width, height, pixelRatio } = currentCanvasMetrics(this.canvas);
    return (
      candidate.geometry.width === width &&
      candidate.geometry.height === height &&
      candidate.geometry.pixelRatio === pixelRatio
    );
  }

  commitCandidate(candidate: PreparedComparisonCanvas): void {
    this.controller?.abort();
    this.controller = null;
    this.pendingBase = null;
    this.pendingComparison = null;
    this.pendingRegion = null;
    this.pendingPolicyPaintKey = null;
    this.geometry = candidate.geometry;
    this.baseComparison = null;
    this.baseRegion = null;
    this.basePolicyPaintKey = null;
    this.paintedTransactionId = null;
    this.desiredTransactionId = null;
    this.releaseBaseBacking();
    delete this.canvas.dataset.renderedRegion;
    delete this.canvas.dataset.renderedTransaction;
  }

  private baseMatches(
    comparison: CurrentComparison,
    selectedRegion: ComparisonRegionKey,
    currentPolicyPaintKey: string,
  ): boolean {
    const { width, height, pixelRatio } = currentCanvasMetrics(this.canvas);
    return (
      this.baseComparison === comparison &&
      this.baseRegion === selectedRegion &&
      this.basePolicyPaintKey === currentPolicyPaintKey &&
      this.geometry?.width === width &&
      this.geometry.height === height &&
      this.geometry.pixelRatio === pixelRatio &&
      this.baseCanvas.width === this.canvas.width &&
      this.baseCanvas.height === this.canvas.height
    );
  }

  private captureBase(): boolean {
    if (
      this.canvas.width < 1 ||
      this.canvas.height < 1 ||
      this.canvas.width * this.canvas.height > MAX_RETAINED_CANVAS_PIXELS
    ) {
      this.releaseBaseBacking();
      return false;
    }
    this.releaseBaseBacking();
    this.baseCanvas.width = this.canvas.width;
    this.baseCanvas.height = this.canvas.height;
    const context = this.baseCanvas.getContext("2d");
    if (context === null) {
      this.releaseBaseBacking();
      return false;
    }
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.globalAlpha = 1;
    context.clearRect(0, 0, this.baseCanvas.width, this.baseCanvas.height);
    context.drawImage(this.canvas, 0, 0);
    return true;
  }

  private paintActiveTransaction(activeTransactionId: string | null): void {
    if (
      this.geometry === null ||
      this.paintedTransactionId === activeTransactionId
    ) {
      return;
    }
    const { context } = prepareCanvasBacking(this.canvas, "whole-pixel");
    context.save();
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.globalAlpha = 1;
    context.clearRect(0, 0, this.canvas.width, this.canvas.height);
    context.drawImage(this.baseCanvas, 0, 0);
    context.restore();
    paintActiveComparisonTransaction(
      context,
      this.geometry.layout,
      activeTransactionId,
    );
    this.paintedTransactionId = activeTransactionId;
    if (activeTransactionId === null) {
      delete this.canvas.dataset.renderedTransaction;
    } else {
      this.canvas.dataset.renderedTransaction = activeTransactionId;
    }
  }

  render(
    comparison: CurrentComparison,
    selectedRegion: ComparisonRegionKey,
    activeTransactionId: string | null,
    policySide: ComparisonSide,
    policyFilter: ComparisonPolicyFilter,
  ): Promise<ComparisonCanvasRenderStatus> {
    this.desiredTransactionId = activeTransactionId;
    if (
      (this.baseComparison !== null && this.baseComparison !== comparison) ||
      (this.pendingComparison !== null && this.pendingComparison !== comparison)
    ) {
      this.releaseBaseBacking();
    }
    const currentPolicyPaintKey = policyPaintKey(policySide, policyFilter);
    if (this.baseMatches(comparison, selectedRegion, currentPolicyPaintKey)) {
      this.paintActiveTransaction(activeTransactionId);
      return Promise.resolve("rendered");
    }
    if (
      this.pendingBase !== null &&
      this.pendingComparison === comparison &&
      this.pendingRegion === selectedRegion &&
      this.pendingPolicyPaintKey === currentPolicyPaintKey
    ) {
      return this.pendingBase;
    }
    this.controller?.abort();
    const controller = new AbortController();
    this.controller = controller;
    this.baseComparison = null;
    this.baseRegion = selectedRegion;
    this.basePolicyPaintKey = null;
    this.paintedTransactionId = null;
    this.pendingComparison = comparison;
    this.pendingRegion = selectedRegion;
    this.pendingPolicyPaintKey = currentPolicyPaintKey;
    let pending!: Promise<ComparisonCanvasRenderStatus>;
    pending = (async () => {
      try {
        const result = await renderComparisonCanvasProgressively(
          this.canvas,
          comparison,
          selectedRegion,
          policySide,
          policyFilter,
          null,
          this.geometry,
          controller.signal,
        );
        if (this.controller !== controller) return "superseded";
        this.geometry = result.geometry;
        const retainedBase = this.captureBase();
        this.baseComparison = retainedBase ? comparison : null;
        this.baseRegion = retainedBase ? selectedRegion : null;
        this.basePolicyPaintKey = retainedBase ? currentPolicyPaintKey : null;
        this.canvas.dataset.renderedRegion = selectedRegion;
        if (retainedBase) {
          this.paintActiveTransaction(this.desiredTransactionId);
        } else {
          const context = this.canvas.getContext("2d");
          if (context === null) {
            throw new Error("Canvas 2D rendering is unavailable");
          }
          context.setTransform(
            result.geometry.pixelRatio,
            0,
            0,
            result.geometry.pixelRatio,
            0,
            0,
          );
          paintActiveComparisonTransaction(
            context,
            result.geometry.layout,
            this.desiredTransactionId,
          );
          this.paintedTransactionId = this.desiredTransactionId;
          if (this.desiredTransactionId === null) {
            delete this.canvas.dataset.renderedTransaction;
          } else {
            this.canvas.dataset.renderedTransaction = this.desiredTransactionId;
          }
        }
        return "rendered";
      } catch (error) {
        if (error instanceof DOMException && error.name === "AbortError")
          return "superseded";
        throw error;
      } finally {
        if (this.controller === controller) this.controller = null;
        if (this.pendingBase === pending) {
          this.pendingBase = null;
          this.pendingComparison = null;
          this.pendingRegion = null;
          this.pendingPolicyPaintKey = null;
        }
      }
    })();
    this.pendingBase = pending;
    return pending;
  }
}
