import {
  paintActiveComparisonTransaction,
  resolveComparisonGeometry,
  renderComparisonCanvasProgressively,
  type ComparisonGeometry,
  type ComparisonLayout,
} from "./comparison-layout";
import { prepareCanvasBacking } from "./canvas-backing";
import type {
  ComparisonRegionKey,
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

export class ComparisonCanvasView {
  private geometry: ComparisonGeometry | null = null;
  private controller: AbortController | null = null;
  private readonly baseCanvas = document.createElement("canvas");
  private baseComparison: CurrentComparison | null = null;
  private baseRegion: ComparisonRegionKey | null = null;
  private paintedTransactionId: string | null = null;
  private desiredTransactionId: string | null = null;
  private pendingBase: Promise<ComparisonCanvasRenderStatus> | null = null;
  private pendingComparison: CurrentComparison | null = null;
  private pendingRegion: ComparisonRegionKey | null = null;

  constructor(private readonly canvas: HTMLCanvasElement) {}

  get layout(): ComparisonLayout | null {
    return this.geometry?.layout ?? null;
  }

  invalidate(): void {
    this.controller?.abort();
    this.controller = null;
    this.pendingBase = null;
    this.pendingComparison = null;
    this.pendingRegion = null;
    this.geometry = null;
    this.baseComparison = null;
    this.baseRegion = null;
    this.paintedTransactionId = null;
    this.desiredTransactionId = null;
    delete this.canvas.dataset.renderedRegion;
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
    this.geometry = candidate.geometry;
    this.baseComparison = null;
    this.baseRegion = null;
    this.paintedTransactionId = null;
    this.desiredTransactionId = null;
    delete this.canvas.dataset.renderedRegion;
  }

  private baseMatches(
    comparison: CurrentComparison,
    selectedRegion: ComparisonRegionKey,
  ): boolean {
    const { width, height, pixelRatio } = currentCanvasMetrics(this.canvas);
    return (
      this.baseComparison === comparison &&
      this.baseRegion === selectedRegion &&
      this.geometry?.width === width &&
      this.geometry.height === height &&
      this.geometry.pixelRatio === pixelRatio
    );
  }

  private captureBase(): void {
    this.baseCanvas.width = this.canvas.width;
    this.baseCanvas.height = this.canvas.height;
    const context = this.baseCanvas.getContext("2d");
    if (context === null) {
      throw new Error("Canvas 2D rendering is unavailable");
    }
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.globalAlpha = 1;
    context.clearRect(0, 0, this.baseCanvas.width, this.baseCanvas.height);
    context.drawImage(this.canvas, 0, 0);
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
  }

  render(
    comparison: CurrentComparison,
    selectedRegion: ComparisonRegionKey,
    activeTransactionId: string | null,
  ): Promise<ComparisonCanvasRenderStatus> {
    this.desiredTransactionId = activeTransactionId;
    if (this.baseMatches(comparison, selectedRegion)) {
      this.paintActiveTransaction(activeTransactionId);
      return Promise.resolve("rendered");
    }
    if (
      this.pendingBase !== null &&
      this.pendingComparison === comparison &&
      this.pendingRegion === selectedRegion
    ) {
      return this.pendingBase;
    }
    this.controller?.abort();
    const controller = new AbortController();
    this.controller = controller;
    this.baseComparison = null;
    this.baseRegion = selectedRegion;
    this.paintedTransactionId = null;
    this.pendingComparison = comparison;
    this.pendingRegion = selectedRegion;
    let pending!: Promise<ComparisonCanvasRenderStatus>;
    pending = (async () => {
      try {
        const result = await renderComparisonCanvasProgressively(
          this.canvas,
          comparison,
          selectedRegion,
          null,
          this.geometry,
          controller.signal,
        );
        if (this.controller !== controller) return "superseded";
        this.geometry = result.geometry;
        this.captureBase();
        this.baseComparison = comparison;
        this.baseRegion = selectedRegion;
        this.canvas.dataset.renderedRegion = selectedRegion;
        this.paintActiveTransaction(this.desiredTransactionId);
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
        }
      }
    })();
    this.pendingBase = pending;
    return pending;
  }
}
