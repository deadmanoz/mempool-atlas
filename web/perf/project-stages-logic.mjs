export const assertPinnedNodeRuntime = (
  actualVersion,
  expectedVersion = "22.23.2",
) => {
  if (actualVersion !== expectedVersion) {
    throw new Error(
      `staged projection requires Node.js ${expectedVersion} for reproducible gzip evidence; current runtime is ${actualVersion}`,
    );
  }
};

export const maximumCandidate = (candidates, field) =>
  candidates.reduce((maximum, candidate) =>
    candidate[field] > maximum[field] ? candidate : maximum,
  );

export const projectWallClockMs = (
  bytes,
  throughputBytesPerSecond,
  requestSeconds,
  processingSeconds,
) =>
  Math.ceil(
    (requestSeconds + bytes / throughputBytesPerSecond + processingSeconds) *
      1_000,
  );

export const composeProjectionGates = (measurements, gates) => {
  const checks = {
    node_primary: measurements.nodePrimaryBytes <= gates.node_primary_bytes,
    node_complete_target:
      measurements.nodeCompleteBytes <= gates.node_complete_target_bytes,
    node_complete_maximum:
      measurements.nodeCompleteBytes <= gates.node_complete_maximum_bytes,
    comparison_primary:
      measurements.comparisonPrimaryBytes <= gates.comparison_primary_bytes,
    comparison_complete_target:
      measurements.comparisonCompleteBytes <=
      gates.comparison_complete_target_bytes,
    comparison_complete_maximum:
      measurements.comparisonCompleteBytes <=
      gates.comparison_complete_maximum_bytes,
  };

  return {
    checks,
    release_gate_passed:
      checks.node_primary &&
      checks.node_complete_maximum &&
      checks.comparison_primary &&
      checks.comparison_complete_maximum,
    requires_pre_authorized_rederivation:
      !checks.node_complete_target || !checks.comparison_complete_target,
  };
};
