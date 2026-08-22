import {
  signatureLabel,
  unknownRulesLabel,
  violationSignature,
} from "./terrain";
import type { Bip110Assessment } from "./types";

export const comparisonAssessmentText = (
  assessment: Bip110Assessment,
): string => {
  if (assessment.status === "compatible") {
    return "Compatible with the deployed Knots mempool policy";
  }
  if (assessment.status === "indeterminate") {
    return "Indeterminate because one or more rule checks remain unresolved";
  }
  const signature = violationSignature(assessment);
  if (signature === null) return "Policy assessment unavailable";
  return signature.completeness === "exact"
    ? `Would violate exactly ${signatureLabel(signature)}`
    : `${signatureLabel(signature)}; unresolved ${unknownRulesLabel(signature)}`;
};

export const compactComparisonEvidence = (values: unknown[]): string => {
  if (values.length === 0) return "";
  const encoded = JSON.stringify(values[0]) ?? "unavailable exemplar";
  return values.length === 1
    ? encoded
    : `${encoded} · exemplar of ${values.length}`;
};
