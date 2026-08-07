const ALPHABET =
  "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const SHAPE =
  /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;

export const decodeCanonicalBase64 = (
  value: unknown,
  field: string,
): Uint8Array => {
  if (typeof value !== "string") throw new TypeError(`Invalid ${field}`);
  if (!SHAPE.test(value)) throw new TypeError(`Non-canonical ${field}`);
  if (
    (value.endsWith("==") &&
      (ALPHABET.indexOf(value.at(-3) ?? "") & 0x0f) !== 0) ||
    (value.endsWith("=") &&
      !value.endsWith("==") &&
      (ALPHABET.indexOf(value.at(-2) ?? "") & 0x03) !== 0)
  ) {
    throw new TypeError(`Non-canonical ${field}`);
  }
  let binary: string;
  try {
    binary = atob(value);
  } catch {
    throw new TypeError(`Invalid ${field}`);
  }
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
};
