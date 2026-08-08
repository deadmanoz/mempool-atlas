import {
  paintBucketTerrainSelection,
  type BucketTerrainLayout,
} from "./bucket-terrain";
import { MAX_RETAINED_CANVAS_PIXELS } from "./canvas-backing";

/**
 * Preserve the last complete terrain paint so a transaction-only selection
 * can move its highlight without clearing and replaying the full population.
 */
export class TerrainSelectionView {
  private readonly baseCanvas = document.createElement("canvas");
  private baseLayout: object | null = null;

  constructor(private readonly canvas: HTMLCanvasElement) {}

  reset(): void {
    this.baseLayout = null;
    this.baseCanvas.width = 0;
    this.baseCanvas.height = 0;
  }

  capture<SectionKey extends string, RegionKey extends string, Signature>(
    layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  ): boolean {
    if (
      this.canvas.width * this.canvas.height > MAX_RETAINED_CANVAS_PIXELS ||
      this.canvas.width < 1 ||
      this.canvas.height < 1
    ) {
      this.reset();
      return false;
    }
    this.reset();
    this.baseCanvas.width = this.canvas.width;
    this.baseCanvas.height = this.canvas.height;
    const context = this.baseCanvas.getContext("2d");
    if (context === null) {
      this.reset();
      return false;
    }
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.globalAlpha = 1;
    context.drawImage(this.canvas, 0, 0);
    this.baseLayout = layout;
    return true;
  }

  paint<SectionKey extends string, RegionKey extends string, Signature>(
    layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
    selectedTxid: string | null,
  ): boolean {
    if (
      this.baseLayout !== layout ||
      this.baseCanvas.width !== this.canvas.width ||
      this.baseCanvas.height !== this.canvas.height
    ) {
      return false;
    }
    const context = this.canvas.getContext("2d");
    if (context === null) return false;
    context.save();
    context.setTransform(1, 0, 0, 1, 0, 0);
    context.globalAlpha = 1;
    context.drawImage(this.baseCanvas, 0, 0);
    context.restore();
    context.setTransform(
      this.canvas.width / layout.width,
      0,
      0,
      this.canvas.height / layout.height,
      0,
      0,
    );
    if (selectedTxid !== null) {
      paintBucketTerrainSelection(context, layout, selectedTxid);
    }
    return true;
  }
}
