import { countFormat } from "./format";
import type { ClassificationResult, ClassifierDescriptor } from "./types";

export interface DetectionPresentation {
  label: string;
  summary: string;
}

const EVIDENCE_CLASSIFIERS = new Set(["data_protocols", "data_carriage_shape"]);

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

const boundedText = (value: unknown): string | null =>
  typeof value === "string" && value.length > 0 ? value.slice(0, 80) : null;

const boundedInteger = (value: unknown): number | null =>
  typeof value === "number" && Number.isSafeInteger(value) && value >= 0
    ? value
    : null;

const carrierLabel = (carrier: string): string =>
  (
    ({
      witness: "witness data",
      tapscript: "Tapscript",
      p2wsh: "P2WSH witness script",
      olga_p2wsh: "P2WSH output fields",
      p2tr_output_key: "P2TR output key",
      op_return: "OP_RETURN",
      bare_multisig: "bare multisig outputs",
    }) as Record<string, string>
  )[carrier] ?? carrier.replaceAll("_", " ");

const framingLabel = (framing: string): string =>
  (
    ({
      classic_if: "classic conditional envelope",
      push_drop: "push/drop envelope",
      op_plenty_v2: "OP_PLENTY v2 framing",
      jxl_n_hide: "JXL-n-hide framing",
    }) as Record<string, string>
  )[framing] ?? framing.replaceAll("_", " ");

const formatLabel = (format: string): string =>
  (
    ({
      pdf: "PDF",
      png: "PNG",
      gif87a: "GIF87a",
      gif89a: "GIF89a",
      wasm_v1: "WebAssembly v1",
      jpeg_jfif: "JFIF JPEG",
      iso_bmff_isom: "ISO BMFF (isom)",
      iso_bmff_iso2: "ISO BMFF (iso2)",
      iso_bmff_mp41: "ISO BMFF (mp41)",
      iso_bmff_mp42: "ISO BMFF (mp42)",
      jpeg_xl_container: "JPEG XL container",
    }) as Record<string, string>
  )[format] ?? format.replaceAll("_", " ");

const locationParts = (detection: Record<string, unknown>): string[] => {
  const parts: string[] = [];
  const carrier = boundedText(detection.carrier);
  if (carrier !== null) parts.push(carrierLabel(carrier));

  const input = boundedInteger(detection.input);
  if (input !== null) parts.push(`input ${countFormat.format(input)}`);
  const output = boundedInteger(detection.output);
  if (output !== null) parts.push(`output ${countFormat.format(output)}`);
  const firstOutput = boundedInteger(detection.first_output);
  const outputCount = boundedInteger(detection.output_count);
  if (firstOutput !== null && outputCount !== null) {
    parts.push(
      `${countFormat.format(outputCount)} outputs from ${countFormat.format(firstOutput)}`,
    );
  }
  const element =
    boundedInteger(detection.element) ??
    boundedInteger(detection.script_element);
  if (element !== null) {
    parts.push(`witness element ${countFormat.format(element)}`);
  }
  return parts;
};

const detectionSummary = (detection: Record<string, unknown>): string => {
  const parts = locationParts(detection);
  const framing = boundedText(detection.framing);
  if (framing !== null) parts.push(framingLabel(framing));

  const pushedElements = boundedInteger(detection.pushed_elements);
  if (pushedElements !== null) {
    parts.push(`${countFormat.format(pushedElements)} pushed elements`);
  }
  const witnessItems = boundedInteger(detection.witness_items);
  if (witnessItems !== null) {
    parts.push(`${countFormat.format(witnessItems)} witness items`);
  }
  const payloadBytes = boundedInteger(detection.payload_bytes);
  const pushedBytes = boundedInteger(detection.pushed_bytes);
  if (payloadBytes !== null) {
    parts.push(`${countFormat.format(payloadBytes)} payload bytes`);
  } else if (pushedBytes !== null) {
    parts.push(`${countFormat.format(pushedBytes)} pushed bytes`);
  }

  const format = boundedText(detection.format);
  const offset = boundedInteger(detection.offset);
  if (format !== null) {
    parts.push(
      offset === null
        ? `${formatLabel(format)} signature`
        : `${formatLabel(format)} signature at raw byte ${countFormat.format(offset)}`,
    );
  }
  if (boundedText(detection.reason) === "invalid_xonly_public_key") {
    parts.push("32-byte key is not a valid x-only public key");
  }

  const marker = boundedText(detection.marker);
  if (marker !== null) parts.push(`marker ${marker}`);
  const confidence = boundedText(detection.confidence);
  if (confidence !== null) parts.push(`${confidence} confidence`);

  return parts.length > 0 ? parts.join(" · ") : "Bounded matching evidence";
};

export const classifierDetectionPresentations = (
  result: ClassificationResult,
  descriptor: ClassifierDescriptor | null,
): DetectionPresentation[] => {
  if (!EVIDENCE_CLASSIFIERS.has(result.classifier_id)) return [];
  if (
    !isRecord(result.evidence) ||
    !Array.isArray(result.evidence.detections)
  ) {
    return [];
  }
  return result.evidence.detections.flatMap((value, index) => {
    if (!isRecord(value)) return [];
    const key = boundedText(value.label);
    const label =
      descriptor?.labels.find(({ key: candidate }) => candidate === key)
        ?.label ??
      key?.replaceAll("_", " ") ??
      "Detection";
    return [
      {
        label: `Detection ${countFormat.format(index + 1)} · ${label}`,
        summary: detectionSummary(value),
      },
    ];
  });
};

export const missingClassifierFactsText = (
  result: ClassificationResult,
): string | null => {
  if (result.missing_facts.length === 0) return null;
  return result.missing_facts
    .map((fact) =>
      fact === "input_script_pubkeys"
        ? "spent-output scripts for one or more inputs"
        : fact.replaceAll("_", " "),
    )
    .join(" · ");
};

export const classifierEvidencePairs = (
  result: ClassificationResult,
  descriptor: ClassifierDescriptor | null,
): [string, string][] => {
  const missing = missingClassifierFactsText(result);
  return [
    ...(missing === null
      ? []
      : [["Missing facts", missing] as [string, string]]),
    ...classifierDetectionPresentations(result, descriptor).map(
      ({ label, summary }): [string, string] => [label, summary],
    ),
  ];
};
