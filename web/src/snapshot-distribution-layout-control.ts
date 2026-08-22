const STORAGE_KEY = "mempool-atlas.snapshot-distribution-columns";
const COLUMN_VALUES = ["auto", "1", "2", "3"] as const;

type DistributionColumnCount = (typeof COLUMN_VALUES)[number];

const columnCount = (value: string | null): DistributionColumnCount =>
  COLUMN_VALUES.find((candidate) => candidate === value) ?? "auto";

const storedColumnCount = (): DistributionColumnCount => {
  try {
    return columnCount(window.localStorage.getItem(STORAGE_KEY));
  } catch {
    return "auto";
  }
};

const storeColumnCount = (value: DistributionColumnCount): void => {
  try {
    window.localStorage.setItem(STORAGE_KEY, value);
  } catch {
    // The layout still applies for this page when storage is unavailable.
  }
};

const requiredButton = (
  root: HTMLElement,
  value: DistributionColumnCount,
): HTMLButtonElement => {
  const button = root.querySelector(`#distribution-columns-${value}`);
  if (!(button instanceof HTMLButtonElement)) {
    throw new Error(
      `Missing required element #snapshot-distributions #distribution-columns-${value}`,
    );
  }
  return button;
};

export const installSnapshotDistributionLayoutControl = (
  root: HTMLElement,
  grid: HTMLElement,
  onLayoutChange: () => void,
): void => {
  const buttons = COLUMN_VALUES.map((value) => ({
    value,
    button: requiredButton(root, value),
  }));
  const apply = (value: DistributionColumnCount): void => {
    grid.dataset.columns = value;
    for (const candidate of buttons) {
      candidate.button.setAttribute(
        "aria-pressed",
        String(candidate.value === value),
      );
    }
    onLayoutChange();
  };

  for (const candidate of buttons) {
    candidate.button.addEventListener("click", () => {
      apply(candidate.value);
      storeColumnCount(candidate.value);
    });
  }
  apply(storedColumnCount());
};
