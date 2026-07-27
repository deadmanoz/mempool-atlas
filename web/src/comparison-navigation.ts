export type ComparisonCursorMove =
  "first" | "last" | "next" | "previous" | "page_next" | "page_previous";

export const moveComparisonCursor = (
  currentIndex: number,
  itemCount: number,
  move: ComparisonCursorMove,
  pageSize = 100,
): number | null => {
  if (itemCount <= 0) {
    return null;
  }
  const lastIndex = itemCount - 1;
  const safeIndex = Math.min(lastIndex, Math.max(0, currentIndex));
  const safePageSize = Math.max(1, pageSize);
  switch (move) {
    case "first":
      return 0;
    case "last":
      return lastIndex;
    case "next":
      return Math.min(lastIndex, safeIndex + 1);
    case "previous":
      return Math.max(0, safeIndex - 1);
    case "page_next":
      return Math.min(lastIndex, safeIndex + safePageSize);
    case "page_previous":
      return Math.max(0, safeIndex - safePageSize);
  }
};

export const cursorMoveForKey = (key: string): ComparisonCursorMove | null => {
  switch (key) {
    case "ArrowDown":
    case "ArrowRight":
      return "next";
    case "ArrowUp":
    case "ArrowLeft":
      return "previous";
    case "Home":
      return "first";
    case "End":
      return "last";
    case "PageDown":
      return "page_next";
    case "PageUp":
      return "page_previous";
    default:
      return null;
  }
};
