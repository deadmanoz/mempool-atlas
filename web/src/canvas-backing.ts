export type CanvasVisibleSizeMode = "subpixel" | "whole-pixel";

export interface PreparedCanvasBacking {
  context: CanvasRenderingContext2D;
  width: number;
  height: number;
  pixelRatio: number;
}

const visibleDimension = (value: number, mode: CanvasVisibleSizeMode): number =>
  Math.max(1, mode === "whole-pixel" ? Math.round(value) : value);

/**
 * Prepare only the visible size, backing store, and logical-pixel transform.
 * Layout, geometry caching, hit testing, and painting remain renderer-owned.
 */
export const prepareCanvasBacking = (
  canvas: HTMLCanvasElement,
  sizeMode: CanvasVisibleSizeMode,
): PreparedCanvasBacking => {
  const bounds = canvas.getBoundingClientRect();
  const width = visibleDimension(bounds.width, sizeMode);
  const height = visibleDimension(bounds.height, sizeMode);
  const pixelRatio = Math.max(1, window.devicePixelRatio || 1);
  const backingWidth = Math.round(width * pixelRatio);
  const backingHeight = Math.round(height * pixelRatio);
  if (canvas.width !== backingWidth) {
    canvas.width = backingWidth;
  }
  if (canvas.height !== backingHeight) {
    canvas.height = backingHeight;
  }
  const context = canvas.getContext("2d");
  if (context === null) {
    throw new Error("Canvas 2D rendering is unavailable");
  }
  context.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0);
  return { context, width, height, pixelRatio };
};
