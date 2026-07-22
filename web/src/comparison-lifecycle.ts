export interface ComparisonRequestTicket {
  generation: number;
  sources: string[];
}

const sameOrder = (
  left: readonly string[],
  right: readonly string[],
): boolean =>
  left.length === right.length &&
  left.every((sourceId, index) => sourceId === right[index]);

/**
 * Tracks the newest comparison request without depending on the DOM. Every
 * selection change and request start advances the generation, so late results
 * cannot overwrite a newer order or a newer refresh of the same order.
 */
export class ComparisonLifecycle {
  private generation = 0;

  invalidate(): void {
    this.generation += 1;
  }

  begin(sources: readonly string[]): ComparisonRequestTicket {
    this.generation += 1;
    return { generation: this.generation, sources: [...sources] };
  }

  isCurrent(
    ticket: ComparisonRequestTicket,
    sources: readonly string[],
  ): boolean {
    return (
      ticket.generation === this.generation &&
      sameOrder(ticket.sources, sources)
    );
  }
}
