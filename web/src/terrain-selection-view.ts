import type { BucketTerrainLayout } from "./bucket-terrain";

/**
 * Position transaction focus above the population. Selection never touches,
 * copies, or replays the dense terrain bitmap.
 */
export class TerrainSelectionView {
  private baseLayout: object | null = null;

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly selectionMarker: HTMLElement,
  ) {}

  private clear(): void {
    this.selectionMarker.hidden = true;
    this.selectionMarker.style.removeProperty("left");
    this.selectionMarker.style.removeProperty("top");
    this.selectionMarker.style.removeProperty("width");
    this.selectionMarker.style.removeProperty("height");
  }

  reset(): void {
    this.baseLayout = null;
    this.clear();
  }

  capture<SectionKey extends string, RegionKey extends string, Signature>(
    layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
  ): boolean {
    const bounds = this.canvas.getBoundingClientRect();
    if (bounds.width <= 0 || bounds.height <= 0) {
      this.reset();
      return false;
    }
    this.baseLayout = layout;
    this.clear();
    return true;
  }

  paint<SectionKey extends string, RegionKey extends string, Signature>(
    layout: BucketTerrainLayout<SectionKey, RegionKey, Signature>,
    selectedTxid: string | null,
  ): boolean {
    if (
      this.baseLayout !== layout ||
      this.canvas.getBoundingClientRect().width <= 0 ||
      this.canvas.getBoundingClientRect().height <= 0
    ) {
      return false;
    }
    const glyph =
      selectedTxid === null
        ? undefined
        : layout.glyphs.find(({ txid }) => txid === selectedTxid);
    if (glyph === undefined) {
      this.clear();
      return true;
    }
    this.selectionMarker.style.left = `${(glyph.rect.x / layout.width) * 100}%`;
    this.selectionMarker.style.top = `${(glyph.rect.y / layout.height) * 100}%`;
    this.selectionMarker.style.width = `${(glyph.rect.width / layout.width) * 100}%`;
    this.selectionMarker.style.height = `${(glyph.rect.height / layout.height) * 100}%`;
    this.selectionMarker.hidden = false;
    return true;
  }
}
