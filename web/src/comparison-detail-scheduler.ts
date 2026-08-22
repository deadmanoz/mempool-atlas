/**
 * Coalesces keyboard-driven transaction detail loads while preserving an
 * immediate path for direct selections such as search and pointer clicks.
 */
export class ComparisonDetailScheduler<T> {
  private timeout: ReturnType<typeof globalThis.setTimeout> | null = null;

  constructor(
    private readonly load: (value: T) => void,
    private readonly settleDelayMs: number,
  ) {}

  schedule(value: T): void {
    this.cancel();
    this.timeout = globalThis.setTimeout(() => {
      this.timeout = null;
      this.load(value);
    }, this.settleDelayMs);
  }

  loadNow(value: T): void {
    this.cancel();
    this.load(value);
  }

  cancel(): void {
    if (this.timeout === null) {
      return;
    }
    globalThis.clearTimeout(this.timeout);
    this.timeout = null;
  }
}
