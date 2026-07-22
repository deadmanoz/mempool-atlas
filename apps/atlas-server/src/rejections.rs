//! Server-side assembly of the source-scoped rejection read model.
//!
//! Rejection evidence is point-in-time, not current state: a
//! `mempool_rejected` observation records that the source's policy refused a
//! transaction at one instant. It is never conflated with
//! `mempool_removed` (expiry, replacement, mining, or eviction), and absence
//! of a rejection is never read as acceptance. The aggregate therefore covers
//! a bounded recent window rather than an unbounded all-time total, and the
//! `recent` list is a separately paginated slice of the same rejection stream.
//!
//! Classification attribution is best-effort and joined at read time from the
//! global txid-intrinsic derivations. The raw bytes may have been observed by
//! any source before or after the refusal. Rejections without a derivation are
//! reported as an explicit unclassified count, never guessed. Within the
//! classified set, a transaction with no stored verdict for a taxonomy takes
//! that taxonomy's honest `unknown` verdict, mirroring the summary engine's
//! effective-verdict rule.

use std::collections::HashMap;

use atlas_model::{
    RejectionAttribution, RejectionReasonCount, RejectionRecord, RejectionTaxonomyBreakdown,
    RejectionVerdictCount, RejectionWindow, SourceId, SourceRejections, TaxonomyDescriptor,
};
use thiserror::Error;

/// The aggregate window covers the most recent this-many rejections for a
/// source, so payload size and query cost stay bounded no matter how long the
/// source has run.
pub const REJECTION_WINDOW_MAX: usize = 1_000;

/// The most distinct node-provided reason strings the aggregate emits before
/// rolling the remainder into a single trailing `other` bucket.
pub const REJECTION_REASON_MAX: usize = 12;

/// Default and maximum page sizes for the recent-rejection list.
pub const REJECTION_PAGE_DEFAULT: usize = 50;
pub const REJECTION_PAGE_MAX: usize = 200;

/// The effective verdict of a classified rejection with no stored verdict for
/// a taxonomy: the honest `unknown`, never a guess.
const UNKNOWN_VERDICT: &str = "unknown";
/// The single free-form reason bucket that absorbs reasons beyond the cap.
const OTHER_REASON: &str = "other";

/// One rejection event with any classifier verdicts derived for its txid, as
/// read from the store. `verdicts` is empty when no classifier derivation is
/// stored; it carries `(taxonomy key, verdict key)` pairs otherwise.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectionRow {
    pub event_id: String,
    pub observed_at_ms: u64,
    pub txid: String,
    pub reason: String,
    pub verdicts: Vec<(String, String)>,
}

/// Everything the rejection read model needs from the store for one source.
/// `window` is the most recent [`REJECTION_WINDOW_MAX`] rejections newest
/// first; `recent` is the requested page of the same stream, newest first;
/// `next_cursor` points to the last returned row when an older row exists.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectionInputs {
    pub window: Vec<RejectionRow>,
    pub recent: Vec<RejectionRow>,
    pub next_cursor: Option<RejectionCursor>,
}

/// An opaque pagination cursor over the rejection stream, ordered by
/// `(observed_at_ms, event_id)` descending. The wire form is
/// `"{observed_at_ms}:{event_id}"`; event IDs never contain a colon (source
/// and session segments are `[A-Za-z0-9._-]`, the sequence is digits), so the
/// first colon unambiguously splits the two fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectionCursor {
    pub observed_at_ms: u64,
    pub event_id: String,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RejectionCursorError {
    #[error("cursor is not in the form observed_at_ms:event_id")]
    Malformed,
}

impl RejectionCursor {
    /// Parses the wire form `"{observed_at_ms}:{event_id}"`.
    pub fn parse(raw: &str) -> Result<Self, RejectionCursorError> {
        let (observed, event_id) = raw.split_once(':').ok_or(RejectionCursorError::Malformed)?;
        let observed_at_ms = observed
            .parse::<u64>()
            .map_err(|_| RejectionCursorError::Malformed)?;
        if i64::try_from(observed_at_ms).is_err() || !is_canonical_event_id(event_id) {
            return Err(RejectionCursorError::Malformed);
        }
        Ok(Self {
            observed_at_ms,
            event_id: event_id.to_owned(),
        })
    }

    /// The wire form `"{observed_at_ms}:{event_id}"`.
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}:{}", self.observed_at_ms, self.event_id)
    }

    /// Source segment embedded in the canonical event ID.
    #[must_use]
    pub fn source_id(&self) -> &str {
        self.event_id
            .split_once('/')
            .expect("validated event IDs contain a source separator")
            .0
    }
}

/// Event IDs are generated as `source_id/source_session_id/local_sequence`.
/// Identifier segments use the shared ASCII grammar and the sequence is
/// canonical unsigned decimal without leading zeroes.
fn is_canonical_event_id(event_id: &str) -> bool {
    let mut segments = event_id.split('/');
    let (Some(source), Some(session), Some(sequence), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return false;
    };
    let valid_identifier = |value: &str| {
        !value.is_empty()
            && value
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    };
    valid_identifier(source)
        && valid_identifier(session)
        && sequence
            .parse::<u64>()
            .is_ok_and(|parsed| parsed.to_string() == sequence)
}

/// Assembles the source-scoped rejection read model from the store inputs, the
/// registered taxonomy descriptors, and the request instant.
#[must_use]
pub fn compute_rejections(
    source_id: SourceId,
    inputs: &RejectionInputs,
    as_of_ms: u64,
    taxonomies: &[TaxonomyDescriptor],
) -> SourceRejections {
    // The window is newest first, so the first row is the newest rejection and
    // the last is the oldest; both are absent only when the window is empty.
    let window = RejectionWindow {
        count: inputs.window.len() as u64,
        oldest_at_ms: inputs.window.last().map(|row| row.observed_at_ms),
        newest_at_ms: inputs.window.first().map(|row| row.observed_at_ms),
    };
    SourceRejections {
        source_id,
        as_of_ms,
        window,
        by_reason: summarize_reasons(&inputs.window),
        attribution: attribute(&inputs.window, taxonomies),
        recent: inputs.recent.iter().map(rejection_record).collect(),
        next_cursor: inputs.next_cursor.as_ref().map(RejectionCursor::encode),
    }
}

fn rejection_record(row: &RejectionRow) -> RejectionRecord {
    RejectionRecord {
        txid: row.txid.clone(),
        reason: row.reason.clone(),
        observed_at_ms: row.observed_at_ms,
        evidence_event_id: row.event_id.clone(),
        verdicts: row.verdicts.clone(),
    }
}

/// Counts reasons over the window, most frequent first with a reason-ascending
/// tiebreak for determinism, capped at [`REJECTION_REASON_MAX`] distinct
/// reasons. Any remainder is summed into a single trailing `other` bucket,
/// appended only when there is a remainder.
fn summarize_reasons(window: &[RejectionRow]) -> Vec<RejectionReasonCount> {
    let mut counts: HashMap<&str, u64> = HashMap::new();
    for row in window {
        *counts.entry(row.reason.as_str()).or_insert(0) += 1;
    }
    let mut ordered: Vec<(&str, u64)> = counts.into_iter().collect();
    ordered.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));

    if ordered.len() <= REJECTION_REASON_MAX {
        return ordered
            .into_iter()
            .map(|(reason, count)| RejectionReasonCount {
                reason: reason.to_owned(),
                count,
                is_rollup: false,
            })
            .collect();
    }
    let mut result: Vec<RejectionReasonCount> = ordered[..REJECTION_REASON_MAX]
        .iter()
        .map(|(reason, count)| RejectionReasonCount {
            reason: (*reason).to_owned(),
            count: *count,
            is_rollup: false,
        })
        .collect();
    let other: u64 = ordered[REJECTION_REASON_MAX..]
        .iter()
        .map(|(_, count)| *count)
        .sum();
    result.push(RejectionReasonCount {
        reason: OTHER_REASON.to_owned(),
        count: other,
        is_rollup: true,
    });
    result
}

/// Partitions the window into classified and unclassified rejections and, over
/// the classified subset, counts each registered taxonomy's verdicts in
/// declared order (including zeros). A classified transaction with no stored
/// verdict for a taxonomy counts under that taxonomy's `unknown` verdict.
fn attribute(window: &[RejectionRow], taxonomies: &[TaxonomyDescriptor]) -> RejectionAttribution {
    let classified: Vec<&RejectionRow> = window
        .iter()
        .filter(|row| !row.verdicts.is_empty())
        .collect();
    let classified_count = classified.len() as u64;
    let unclassified_count = window.len() as u64 - classified_count;

    let taxonomy_breakdowns = taxonomies
        .iter()
        .map(|taxonomy| {
            let mut counts = vec![0_u64; taxonomy.verdicts.len()];
            for row in &classified {
                let verdict = effective_verdict(&row.verdicts, &taxonomy.key);
                counts[verdict_bin(taxonomy, verdict)] += 1;
            }
            RejectionTaxonomyBreakdown {
                key: taxonomy.key.clone(),
                label: taxonomy.label.clone(),
                verdicts: taxonomy
                    .verdicts
                    .iter()
                    .zip(counts)
                    .map(|(descriptor, count)| RejectionVerdictCount {
                        verdict: descriptor.key.clone(),
                        count,
                    })
                    .collect(),
            }
        })
        .collect();

    RejectionAttribution {
        classified_count,
        unclassified_count,
        taxonomies: taxonomy_breakdowns,
    }
}

/// The stored verdict for one taxonomy, or the honest `unknown` when absent.
fn effective_verdict<'a>(verdicts: &'a [(String, String)], taxonomy_key: &str) -> &'a str {
    verdicts
        .iter()
        .find(|(taxonomy, _)| taxonomy == taxonomy_key)
        .map_or(UNKNOWN_VERDICT, |(_, verdict)| verdict.as_str())
}

/// The bin index of one verdict within a taxonomy's declared vocabulary.
/// Verdicts a pack no longer declares fall into the guaranteed `unknown` bin.
fn verdict_bin(taxonomy: &TaxonomyDescriptor, verdict: &str) -> usize {
    taxonomy
        .verdicts
        .iter()
        .position(|descriptor| descriptor.key == verdict)
        .unwrap_or_else(|| {
            taxonomy
                .verdicts
                .iter()
                .position(|descriptor| descriptor.key == UNKNOWN_VERDICT)
                .expect("every taxonomy declares the unknown verdict")
        })
}

#[cfg(test)]
mod tests {
    use atlas_model::{Classification, VerdictDescriptor};

    use super::*;

    fn source() -> SourceId {
        SourceId::new("source-a").expect("source")
    }

    const AS_OF_MS: u64 = 1_752_710_400_000;

    fn behavior_taxonomy() -> TaxonomyDescriptor {
        TaxonomyDescriptor {
            key: "behavior".to_owned(),
            label: "Behavior".to_owned(),
            verdicts: Classification::ALL
                .into_iter()
                .map(|classification| VerdictDescriptor {
                    key: classification.key().to_owned(),
                    label: classification.label().to_owned(),
                })
                .collect(),
        }
    }

    fn row(
        event_id: &str,
        observed_at_ms: u64,
        reason: &str,
        verdicts: &[(&str, &str)],
    ) -> RejectionRow {
        RejectionRow {
            event_id: event_id.to_owned(),
            observed_at_ms,
            txid: format!("{:064x}", observed_at_ms),
            reason: reason.to_owned(),
            verdicts: verdicts
                .iter()
                .map(|(taxonomy, verdict)| ((*taxonomy).to_owned(), (*verdict).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn cursor_round_trips_through_its_wire_form() {
        let cursor = RejectionCursor {
            observed_at_ms: 1_752_710_000_000,
            event_id: "source-a/session-a/9".to_owned(),
        };
        assert_eq!(cursor.encode(), "1752710000000:source-a/session-a/9");
        assert_eq!(RejectionCursor::parse(&cursor.encode()), Ok(cursor));
    }

    #[test]
    fn malformed_cursors_are_rejected() {
        for raw in [
            "",
            "nocolon",
            "notanumber:source-a/session-a/9",
            "1752710000000:",
            ":source-a/session-a/9",
            "1752710000000:garbage",
            "1752710000000:source-a/session-a/09",
            "1752710000000:source-a/session-a/9/extra",
            "1752710000000:source-a/session-a/9:garbage",
            "1752710000000:source@/session-a/9",
            "18446744073709551615:source-a/session-a/9",
        ] {
            assert_eq!(
                RejectionCursor::parse(raw),
                Err(RejectionCursorError::Malformed),
                "{raw} should be malformed",
            );
        }
    }

    #[test]
    fn empty_window_reports_no_timestamps_and_zeroed_attribution() {
        let rejections = compute_rejections(
            source(),
            &RejectionInputs {
                window: Vec::new(),
                recent: Vec::new(),
                next_cursor: None,
            },
            AS_OF_MS,
            &[behavior_taxonomy()],
        );
        assert_eq!(
            rejections.window,
            RejectionWindow {
                count: 0,
                oldest_at_ms: None,
                newest_at_ms: None,
            }
        );
        assert!(rejections.by_reason.is_empty());
        assert_eq!(rejections.attribution.classified_count, 0);
        assert_eq!(rejections.attribution.unclassified_count, 0);
        // Even an empty window emits the taxonomy shell with zeroed verdicts.
        assert_eq!(rejections.attribution.taxonomies.len(), 1);
        assert!(
            rejections.attribution.taxonomies[0]
                .verdicts
                .iter()
                .all(|verdict| verdict.count == 0)
        );
        assert!(rejections.recent.is_empty());
        assert!(rejections.next_cursor.is_none());
    }

    #[test]
    fn window_timestamps_follow_newest_first_ordering() {
        let window = vec![
            row("s/x/3", 300, "insufficient fee", &[]),
            row("s/x/2", 200, "insufficient fee", &[]),
            row("s/x/1", 100, "dust", &[]),
        ];
        let rejections = compute_rejections(
            source(),
            &RejectionInputs {
                window,
                recent: Vec::new(),
                next_cursor: None,
            },
            AS_OF_MS,
            &[behavior_taxonomy()],
        );
        assert_eq!(
            rejections.window,
            RejectionWindow {
                count: 3,
                oldest_at_ms: Some(100),
                newest_at_ms: Some(300),
            }
        );
    }

    #[test]
    fn by_reason_distinguishes_literal_other_from_the_overflow_rollup() {
        let mut window = Vec::new();
        let mut sequence = 0;
        let mut push = |reason: &str, times: usize, window: &mut Vec<RejectionRow>| {
            for _ in 0..times {
                sequence += 1;
                window.push(row(
                    &format!("s/x/{sequence}"),
                    1_000 + sequence,
                    reason,
                    &[],
                ));
            }
        };
        // A literal `other` reason is kept as node-provided text while excess
        // distinct reasons roll into a separately marked synthetic bucket.
        push("zzz-hot-reason", 5, &mut window);
        push("other", 4, &mut window);
        for index in 0..14 {
            push(&format!("reason-{index:02}"), 1, &mut window);
        }

        let rejections = compute_rejections(
            source(),
            &RejectionInputs {
                window,
                recent: Vec::new(),
                next_cursor: None,
            },
            AS_OF_MS,
            &[behavior_taxonomy()],
        );
        // Count-descending puts the hot reason first even though it sorts last
        // alphabetically; the remaining singles follow reason-ascending.
        assert_eq!(rejections.by_reason.len(), REJECTION_REASON_MAX + 1);
        assert_eq!(
            rejections.by_reason[0],
            RejectionReasonCount {
                reason: "zzz-hot-reason".to_owned(),
                count: 5,
                is_rollup: false,
            }
        );
        assert_eq!(
            rejections.by_reason[1],
            RejectionReasonCount {
                reason: "other".to_owned(),
                count: 4,
                is_rollup: false,
            }
        );
        assert_eq!(rejections.by_reason[2].reason, "reason-00");
        assert_eq!(rejections.by_reason[11].reason, "reason-09");
        // reason-10 through reason-13 roll into the trailing synthetic bucket.
        assert_eq!(
            rejections.by_reason[REJECTION_REASON_MAX],
            RejectionReasonCount {
                reason: "other".to_owned(),
                count: 4,
                is_rollup: true,
            }
        );
    }

    #[test]
    fn attribution_partitions_and_synthesizes_unknown_for_missing_taxonomies() {
        let window = vec![
            // Classified as behavior=data.
            row("s/x/3", 300, "dust", &[("behavior", "data")]),
            // Classified, but carries no behavior verdict -> counts as unknown.
            row("s/x/2", 200, "dust", &[("bip110", "conforming")]),
            // Unclassified: no verdicts at all.
            row("s/x/1", 100, "dust", &[]),
        ];
        let rejections = compute_rejections(
            source(),
            &RejectionInputs {
                window,
                recent: Vec::new(),
                next_cursor: None,
            },
            AS_OF_MS,
            &[behavior_taxonomy()],
        );
        assert_eq!(rejections.attribution.classified_count, 2);
        assert_eq!(rejections.attribution.unclassified_count, 1);
        let behavior = &rejections.attribution.taxonomies[0];
        assert_eq!(behavior.key, "behavior");
        let count_for = |verdict: &str| {
            behavior
                .verdicts
                .iter()
                .find(|entry| entry.verdict == verdict)
                .unwrap_or_else(|| panic!("behavior declares {verdict}"))
                .count
        };
        assert_eq!(count_for("data"), 1);
        // The classified-but-behaviorless row lands in unknown, not underived.
        assert_eq!(count_for("unknown"), 1);
        assert_eq!(count_for("payment"), 0);
        // Verdict counts over the classified set sum to classified_count.
        let total: u64 = behavior.verdicts.iter().map(|entry| entry.count).sum();
        assert_eq!(total, rejections.attribution.classified_count);
    }

    #[test]
    fn recent_records_carry_their_verdicts_and_next_cursor_encodes() {
        let recent = vec![
            row("s/x/2", 200, "dust", &[("behavior", "data")]),
            row("s/x/1", 100, "insufficient fee", &[]),
        ];
        let next = RejectionCursor {
            observed_at_ms: 100,
            event_id: "s/x/1".to_owned(),
        };
        let rejections = compute_rejections(
            source(),
            &RejectionInputs {
                window: recent.clone(),
                recent,
                next_cursor: Some(next.clone()),
            },
            AS_OF_MS,
            &[behavior_taxonomy()],
        );
        assert_eq!(rejections.recent.len(), 2);
        assert_eq!(
            rejections.recent[0].verdicts,
            vec![("behavior".to_owned(), "data".to_owned())]
        );
        assert!(rejections.recent[1].verdicts.is_empty());
        assert_eq!(rejections.next_cursor, Some(next.encode()));
    }
}
