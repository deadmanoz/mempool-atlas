export const PINNED_THROUGHPUT_BYTES_PER_SECOND = 159_461.45607954692;

export const STAGED_PROJECTION_GATES = Object.freeze({
  node_primary_bytes: 3_189_229,
  node_complete_target_bytes: 6_059_535,
  node_complete_maximum_bytes: 6_537_919,
  comparison_primary_bytes: 5_740_612,
  comparison_complete_target_bytes: 12_119_070,
  comparison_complete_maximum_bytes: 13_075_839,
});

export const RELEASE_GATES = Object.freeze({
  metadata_usable_ms: 5_000,
  maximum_responsiveness_long_task_ms: 200,
  interaction_handler_ms: 200,
  interaction_settle_ms: 5_000,
  bip110_rule_navigation_handler_ms: 200,
  maximum_animation_frame_callback_ms: Object.freeze({
    desktop: 8,
    "mobile-slow-4g": 16,
  }),
  cls: 0.1,
  node: Object.freeze({
    primary_interaction_ms: 30_000,
    complete_feature_ready_ms: 52_000,
    primary_transfer_budget_ms: 20_000,
    complete_transfer_budget_ms: 38_000,
    primary_encoded_body_bytes: STAGED_PROJECTION_GATES.node_primary_bytes,
    complete_encoded_body_bytes:
      STAGED_PROJECTION_GATES.node_complete_target_bytes,
    page_heap_bytes: 40_000_000,
    cross_context_bytes: 60 * 1024 * 1024,
    replacement_retained_bytes: 90 * 1024 * 1024,
    bip110_page_heap_bytes: 90_000_000,
    bip110_cross_context_bytes: 120 * 1024 * 1024,
  }),
  comparison: Object.freeze({
    primary_interaction_ms: 50_000,
    complete_feature_ready_ms: 94_000,
    primary_transfer_budget_ms: 36_000,
    complete_transfer_budget_ms: 76_000,
    primary_encoded_body_bytes:
      STAGED_PROJECTION_GATES.comparison_primary_bytes,
    complete_encoded_body_bytes:
      STAGED_PROJECTION_GATES.comparison_complete_target_bytes,
    page_heap_bytes: 60_000_000,
    cross_context_bytes: 100 * 1024 * 1024,
    replacement_retained_bytes: 150 * 1024 * 1024,
    bip110_page_heap_bytes: null,
    bip110_cross_context_bytes: null,
  }),
});
