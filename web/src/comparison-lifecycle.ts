export interface RequestTicket {
  generation: number;
  signal: AbortSignal;
}

export const isAbortError = (error: unknown): boolean =>
  typeof error === "object" &&
  error !== null &&
  "name" in error &&
  error.name === "AbortError";

/** Owns one abortable request generation and rejects every older ticket. */
export class RequestLifecycle {
  private generation = 0;
  private controller: AbortController | null = null;

  invalidate(): void {
    this.generation += 1;
    this.controller?.abort();
    this.controller = null;
  }

  begin(): RequestTicket {
    this.invalidate();
    this.controller = new AbortController();
    return {
      generation: this.generation,
      signal: this.controller.signal,
    };
  }

  isCurrent(ticket: RequestTicket): boolean {
    return !ticket.signal.aborted && ticket.generation === this.generation;
  }
}

export interface ComparisonRequestTicket extends RequestTicket {
  leftSourceId: string;
  rightSourceId: string;
}

/** Guards the UI against an older source pair or refresh completing late. */
export class ComparisonLifecycle {
  private readonly requests = new RequestLifecycle();

  invalidate(): void {
    this.requests.invalidate();
  }

  begin(leftSourceId: string, rightSourceId: string): ComparisonRequestTicket {
    const ticket = this.requests.begin();
    return {
      ...ticket,
      leftSourceId,
      rightSourceId,
    };
  }

  isCurrent(
    ticket: ComparisonRequestTicket,
    leftSourceId: string,
    rightSourceId: string,
  ): boolean {
    return (
      this.requests.isCurrent(ticket) &&
      ticket.leftSourceId === leftSourceId &&
      ticket.rightSourceId === rightSourceId
    );
  }
}
