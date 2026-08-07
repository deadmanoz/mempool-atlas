export interface CooperativeWorkOptions {
  batchSize?: number;
  yieldBetweenBatches?: () => void | Promise<void>;
  signal?: AbortSignal;
}

export interface CombinedAbortSignal {
  signal: AbortSignal;
  dispose: () => void;
}

export const DEFAULT_COOPERATIVE_BATCH_SIZE = 750;

export const combineAbortSignals = (
  signals: readonly AbortSignal[],
): CombinedAbortSignal => {
  if (signals.length === 0) {
    throw new RangeError("At least one abort signal is required");
  }
  if (signals.length === 1) {
    return { signal: signals[0]!, dispose: () => undefined };
  }

  const controller = new AbortController();
  const listeners: Array<readonly [AbortSignal, () => void]> = [];
  const dispose = (): void => {
    for (const [signal, listener] of listeners) {
      signal.removeEventListener("abort", listener);
    }
    listeners.length = 0;
  };

  for (const signal of signals) {
    if (signal.aborted) {
      controller.abort(signal.reason);
      dispose();
      break;
    }
    const listener = (): void => {
      controller.abort(signal.reason);
      dispose();
    };
    listeners.push([signal, listener]);
    signal.addEventListener("abort", listener, { once: true });
  }

  return { signal: controller.signal, dispose };
};

const messageChannelResolvers: Array<() => void> = [];
const messageChannel =
  typeof MessageChannel === "undefined" ? null : new MessageChannel();
if (messageChannel !== null) {
  messageChannel.port1.onmessage = () => messageChannelResolvers.shift()?.();
}

const yieldWithoutTimerClamp = (): Promise<void> => {
  if (messageChannel === null) {
    return new Promise((resolve) => setTimeout(resolve, 0));
  }
  return new Promise((resolve) => {
    messageChannelResolvers.push(resolve);
    messageChannel.port2.postMessage(undefined);
  });
};

const yieldToMainThread = (): Promise<void> => {
  const scheduler = (
    globalThis as typeof globalThis & {
      scheduler?: { yield?: () => Promise<void> };
    }
  ).scheduler;
  return scheduler?.yield?.() ?? yieldWithoutTimerClamp();
};

export const yieldCooperatively = async (
  options: CooperativeWorkOptions = {},
): Promise<void> => {
  options.signal?.throwIfAborted();
  await (options.yieldBetweenBatches ?? yieldToMainThread)();
  options.signal?.throwIfAborted();
};

export const forEachCooperatively = async <T>(
  values: readonly T[],
  visit: (value: T, index: number) => void,
  options: CooperativeWorkOptions = {},
): Promise<void> => {
  const batchSize = options.batchSize ?? DEFAULT_COOPERATIVE_BATCH_SIZE;
  if (!Number.isSafeInteger(batchSize) || batchSize <= 0) {
    throw new RangeError("Cooperative batch size must be a positive integer");
  }
  options.signal?.throwIfAborted();
  const length = values.length;
  const iterator = values[Symbol.iterator]();
  let index = 0;
  while (index < length) {
    const end = Math.min(index + batchSize, length);
    while (index < end) {
      const next = iterator.next();
      if (next.done) {
        options.signal?.throwIfAborted();
        return;
      }
      if (next.value !== undefined) visit(next.value, index);
      index += 1;
    }
    if (index < length) {
      await yieldCooperatively(options);
    }
  }
  options.signal?.throwIfAborted();
};
