const ALPHABET =
  "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const isAlphabetCharacter = (code: number): boolean =>
  (code >= 0x41 && code <= 0x5a) ||
  (code >= 0x61 && code <= 0x7a) ||
  (code >= 0x30 && code <= 0x39) ||
  code === 0x2b ||
  code === 0x2f;

const hasCanonicalShape = (value: string): boolean => {
  if (value.length % 4 !== 0) return false;
  const padding = value.endsWith("==") ? 2 : value.endsWith("=") ? 1 : 0;
  const contentLength = value.length - padding;
  for (let index = 0; index < contentLength; index += 1) {
    if (!isAlphabetCharacter(value.charCodeAt(index))) return false;
  }
  return (
    (padding === 0 || contentLength >= 2) &&
    (padding < 2 || contentLength % 4 === 2) &&
    (padding !== 1 || contentLength % 4 === 3)
  );
};

export const decodeCanonicalBase64 = (
  value: unknown,
  field: string,
): Uint8Array => {
  if (typeof value !== "string") throw new TypeError(`Invalid ${field}`);
  if (!hasCanonicalShape(value)) throw new TypeError(`Non-canonical ${field}`);
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
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
};
