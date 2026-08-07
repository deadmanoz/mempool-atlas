export const PINNED_THROUGHPUT_BYTES_PER_SECOND: number;

export const STAGED_PROJECTION_GATES: Readonly<{
  node_primary_bytes: number;
  node_complete_target_bytes: number;
  node_complete_maximum_bytes: number;
  comparison_primary_bytes: number;
  comparison_complete_target_bytes: number;
  comparison_complete_maximum_bytes: number;
}>;

interface ScenarioReleaseGate {
  readonly primary_interaction_ms: number;
  readonly complete_feature_ready_ms: number;
  readonly primary_transfer_budget_ms: number;
  readonly complete_transfer_budget_ms: number;
  readonly primary_encoded_body_bytes: number;
  readonly complete_encoded_body_bytes: number;
  readonly page_heap_bytes: number;
  readonly cross_context_bytes: number;
  readonly replacement_retained_bytes: number;
  readonly bip110_page_heap_bytes: number | null;
  readonly bip110_cross_context_bytes: number | null;
}

export const RELEASE_GATES: Readonly<{
  metadata_usable_ms: number;
  maximum_responsiveness_long_task_ms: number;
  interaction_handler_ms: number;
  interaction_settle_ms: number;
  bip110_rule_navigation_handler_ms: number;
  maximum_animation_frame_callback_ms: Readonly<{
    desktop: number;
    "mobile-slow-4g": number;
  }>;
  cls: number;
  node: ScenarioReleaseGate;
  comparison: ScenarioReleaseGate;
}>;
