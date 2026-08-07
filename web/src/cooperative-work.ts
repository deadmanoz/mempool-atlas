export interface CooperativeWorkOptions {
  batchSize?: number;
  yieldBetweenBatches?: () => void | Promise<void>;
  signal?: AbortSignal;
}

export const DEFAULT_COOPERATIVE_BATCH_SIZE = 750;

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
  for (let start = 0; start < values.length; start += batchSize) {
    const end = Math.min(start + batchSize, values.length);
    for (let index = start; index < end; index += 1) {
      const value = values[index];
      if (value !== undefined) visit(value, index);
    }
    if (end < values.length) {
      await yieldCooperatively(options);
    }
  }
  options.signal?.throwIfAborted();
};
