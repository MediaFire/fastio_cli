//! Offline classification of Fast.io `OpaqueId` strings.
//!
//! A Fast.io `OpaqueId` has the shape `<type><KSUID(26)><CRC(2)>`. The id is
//! **self-describing by length**:
//!
//! - **Non-workflow** ids carry a **1-character** type prefix → **29 chars**.
//! - **Workflow-family** ids carry a **2-character** prefix (a master `w` plus a
//!   sub-type) → **30 chars**.
//!
//! A formatted (display) id inserts a `-` every 5 characters, so the hyphenated
//! forms are 34 (legacy) and 35 (workflow) chars; the raw, dash-free form is
//! canonical for length checks and type reads.
//!
//! This module is a **pure, offline** classifier: it strips hyphens, reads the
//! self-describing length, and looks the type code up in the authoritative
//! type-prefix → entity map. It performs no network or disk I/O and is shared by
//! the `fastio id info` CLI command and the MCP `id` tool.
//!
//! ## The §4 contract (load-bearing)
//!
//! The workflow family is detected **only** by length-30 / leading-`w`. The old
//! single-character workflow codes (`g`, `h`, `j`, …) are **transitional** and
//! will be **reassigned to new, non-workflow types** in a later cleanup. A
//! 29-char id whose 1-char code is not in the documented non-workflow map is
//! therefore reported as `unknown` (not guessed as workflow) — a future entity
//! could legitimately claim that code. New workflow ids are always 30-char
//! under `w`.
//!
//! Source of truth: `~/vividengine/docs/integration/
//! workflow-webhook-opaque-id-30char.md` (§5 type map, §4 caveat).

use serde_json::{Value, json};

/// Whether a client typically receives an id of a given type in a payload.
///
/// `#[non_exhaustive]` because the upstream "surfaced / internal" classification
/// may gain tiers without a CLI-version bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Surfacing {
    /// Routinely surfaced to clients.
    Surfaced,
    /// Sometimes surfaced, depending on the call.
    Sometimes,
    /// Internal — not normally handed to clients.
    Internal,
}

impl Surfacing {
    /// The lowercase token used in serialized output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Surfaced => "surfaced",
            Self::Sometimes => "sometimes",
            Self::Internal => "internal",
        }
    }
}

/// The result of classifying a Fast.io identifier offline.
///
/// `#[non_exhaustive]` so fields can be added without breaking the binary crate,
/// which only ever reads a [`Classification`] (via [`classify`] / [`to_json`])
/// rather than constructing one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Classification {
    /// The input as classified (outer whitespace trimmed).
    pub input: String,
    /// The dash-stripped form actually inspected.
    pub raw: String,
    /// Length of [`raw`](Self::raw) — 29 or 30 for an `OpaqueId`; some other
    /// value for non-`OpaqueId` input.
    pub length: usize,
    /// The type code read from the prefix: 2 chars for the workflow family
    /// (length 30), 1 char otherwise. `None` when the input is not
    /// `OpaqueId`-shaped.
    pub type_code: Option<String>,
    /// The mapped entity type, or `"Unknown"` when unrecognized.
    pub entity_type: &'static str,
    /// The id family: `"workflow"`, `"non-workflow"`, or `"unknown"`.
    pub family: &'static str,
    /// The surfacing tier, present only when the code maps to a known entity.
    pub surfacing: Option<Surfacing>,
    /// Whether the id maps to a concrete, real entity type. A `0` sentinel, an
    /// unmapped code, or non-`OpaqueId` input is `false`.
    pub recognized: bool,
    /// A human-readable note: the §5 entity note, the §4 transitional-code
    /// caveat, or a hint for non-`OpaqueId` input.
    pub note: &'static str,
}

/// Classify a Fast.io identifier offline.
///
/// Strips display hyphens first, then reads the self-describing length (29 →
/// 1-char type, 30 → 2-char `w` type) and looks the type code up in the §5 map.
/// Recognition is case-insensitive and exactly as permissive as the documented
/// server contract `^[a-z0-9]{29,30}$` (it does **not** apply the stricter
/// Crockford base32 alphabet — being stricter than the server would reject ids
/// the server accepts).
///
/// Per the §4 contract, the workflow family is detected ONLY by length-30 /
/// leading-`w`; a 29-char id whose single-char code is unmapped is reported
/// `unknown` (it may be a transitional workflow code pending reassignment)
/// rather than guessed.
#[must_use]
pub fn classify(id: &str) -> Classification {
    let input = id.trim();
    let raw: String = input.chars().filter(|&c| c != '-').collect();
    let length = raw.len();

    // Opaque shape per the server contract: 29 or 30 chars, all ASCII
    // alphanumeric (case-insensitive `[a-z0-9]`). Intentionally NOT stricter
    // than the server (no Crockford i/l/o/u exclusion).
    let opaque_shaped =
        (length == 29 || length == 30) && raw.chars().all(|c| c.is_ascii_alphanumeric());

    if !opaque_shaped {
        let note = if length == 19 && raw.chars().all(|c| c.is_ascii_digit()) {
            "not an OpaqueId — looks like a 19-digit numeric profile id (org / workspace / share)"
        } else {
            "not a Fast.io OpaqueId (expected 29 or 30 base32 chars after stripping hyphens)"
        };
        return Classification {
            input: input.to_owned(),
            raw,
            length,
            type_code: None,
            entity_type: "Unknown",
            family: "unknown",
            surfacing: None,
            recognized: false,
            note,
        };
    }

    // Safe slicing: every char is ASCII alphanumeric (1 byte), so a char-based
    // `take` cannot split a multi-byte boundary.
    let code: String = raw
        .chars()
        .take(if length == 30 { 2 } else { 1 })
        .collect::<String>()
        .to_ascii_lowercase();

    if length == 30 {
        // Workflow family — must lead with `w` AND have a mapped subtype.
        if let Some((entity_type, surfacing, note)) = lookup_workflow(&code) {
            Classification {
                input: input.to_owned(),
                raw,
                length,
                type_code: Some(code),
                entity_type,
                family: "workflow",
                surfacing: Some(surfacing),
                recognized: true,
                note,
            }
        } else {
            let note = if code.starts_with('w') {
                "unrecognized workflow subtype code (length-30, leads with 'w')"
            } else {
                "length-30 but does not lead with 'w' — not a Fast.io workflow OpaqueId"
            };
            Classification {
                input: input.to_owned(),
                raw,
                length,
                type_code: Some(code),
                entity_type: "Unknown",
                family: "unknown",
                surfacing: None,
                recognized: false,
                note,
            }
        }
    } else {
        // length == 29 — non-workflow family (1-char code).
        match lookup_non_workflow(&code) {
            Some((entity_type, surfacing, note)) => Classification {
                input: input.to_owned(),
                raw,
                length,
                type_code: Some(code),
                entity_type,
                family: "non-workflow",
                surfacing: Some(surfacing),
                recognized: true,
                note,
            },
            // `0` is the documented non-workflow sentinel — a known code that is
            // never a real id.
            None if code == "0" => Classification {
                input: input.to_owned(),
                raw,
                length,
                type_code: Some(code),
                entity_type: "Unknown",
                family: "non-workflow",
                surfacing: None,
                recognized: false,
                note: "sentinel code '0' — never a real id",
            },
            // Any other unmapped 1-char code: do NOT guess. Per §4 this may be a
            // transitional single-char workflow code pending reassignment.
            None => Classification {
                input: input.to_owned(),
                raw,
                length,
                type_code: Some(code),
                entity_type: "Unknown",
                family: "unknown",
                surfacing: None,
                recognized: false,
                note: "unmapped 1-char code — NOT classified as workflow: transitional \
                       single-char workflow codes will be reassigned; new workflow ids are \
                       30-char under 'w'",
            },
        }
    }
}

/// Render a [`Classification`] as a JSON object for the output formatters.
///
/// Key order is fixed (and honored because `serde_json`'s `preserve_order`
/// feature is enabled) so the table / CSV / markdown columns stay stable.
#[must_use]
pub fn to_json(c: &Classification) -> Value {
    json!({
        "input": c.input,
        "raw": c.raw,
        "length": c.length,
        "type_code": c.type_code,
        "entity_type": c.entity_type,
        "family": c.family,
        "surfacing": c.surfacing.map(Surfacing::as_str),
        "recognized": c.recognized,
        "note": c.note,
    })
}

/// The §5 workflow-family map: 2-char `w`-prefixed code → (entity, surfacing,
/// note). `WorkflowReview` (`wt`) is intentionally ONE code shared by all seven
/// review tables.
fn lookup_workflow(code: &str) -> Option<(&'static str, Surfacing, &'static str)> {
    let entry = match code {
        "wa" => ("WorkflowStepOccurrence", Surfacing::Surfaced, ""),
        "wd" => ("WorkflowTrigger", Surfacing::Surfaced, ""),
        "wf" => ("WorkflowTemplate", Surfacing::Surfaced, ""),
        "we" => (
            "WorkflowObligation",
            Surfacing::Surfaced,
            "task / approval / inbox item",
        ),
        "wt" => (
            "WorkflowReview",
            Surfacing::Surfaced,
            "one shared code across all 7 review tables; the surrounding payload field tells \
             you which table",
        ),
        "wq" => ("WorkspacePolicy", Surfacing::Sometimes, ""),
        "wp" => ("WorkflowRole", Surfacing::Sometimes, ""),
        "wm" => (
            "WorkflowOutboundSub",
            Surfacing::Sometimes,
            "outbound-webhook subscription",
        ),
        "wb" => ("WorkflowEdge", Surfacing::Internal, ""),
        "wc" => ("WorkflowLoop", Surfacing::Internal, ""),
        "wg" => ("WorkflowTransformHandler", Surfacing::Internal, ""),
        "wh" => ("WorkflowOverlay", Surfacing::Internal, "audit overlay"),
        "wj" => (
            "WorkflowCheckpoint",
            Surfacing::Internal,
            "audit checkpoint",
        ),
        "wk" => ("WorkflowEvidenceSnapshot", Surfacing::Internal, ""),
        "wn" => (
            "WorkflowInboundNonce",
            Surfacing::Internal,
            "inbound-webhook nonce",
        ),
        "wr" => (
            "WorkflowNlLedger",
            Surfacing::Internal,
            "NL audit-ledger entry",
        ),
        "ws" => ("WorkflowPool", Surfacing::Internal, ""),
        _ => return None,
    };
    Some(entry)
}

/// The §5 non-workflow map: 1-char code → (entity, surfacing, note). The `0`
/// sentinel is deliberately absent — it is handled by [`classify`] as an
/// unrecognized "never a real id" code rather than a concrete entity.
fn lookup_non_workflow(code: &str) -> Option<(&'static str, Surfacing, &'static str)> {
    let entry = match code {
        "2" => (
            "StorageNode",
            Surfacing::Surfaced,
            "a file or folder (the \"node\")",
        ),
        "3" => (
            "StorageVersion",
            Surfacing::Surfaced,
            "a specific version of a file",
        ),
        "5" => ("Upload", Surfacing::Surfaced, "an upload session / handle"),
        "6" => ("Share", Surfacing::Surfaced, ""),
        "8" => ("Asset", Surfacing::Surfaced, "a derived / preview asset"),
        "9" => ("AiJob", Surfacing::Sometimes, ""),
        "e" => (
            "OrgTransfer",
            Surfacing::Sometimes,
            "an org-transfer handle",
        ),
        "c" => ("Metadata", Surfacing::Sometimes, "a metadata record"),
        "4" => ("StoragePhy", Surfacing::Internal, "physical storage object"),
        "7" => ("StorageData", Surfacing::Internal, "storage data object"),
        "f" => (
            "ChunkManifest",
            Surfacing::Internal,
            "upload chunk manifest",
        ),
        "b" => (
            "AiTransaction",
            Surfacing::Internal,
            "AI billing / usage transaction",
        ),
        "d" => ("AIAgentKey", Surfacing::Internal, "AI agent key"),
        "a" => ("General", Surfacing::Internal, "general-purpose id"),
        _ => return None,
    };
    Some(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `OpaqueId`-shaped string of exact length `len` from a prefix,
    /// padding with `x` (an ASCII-alphanumeric base32 char).
    fn id_of(prefix: &str, len: usize) -> String {
        let mut s = prefix.to_owned();
        while s.len() < len {
            s.push('x');
        }
        assert_eq!(s.len(), len);
        s
    }

    /// Re-insert a `-` every 5 chars to produce the formatted (display) form.
    fn hyphenate(raw: &str) -> String {
        raw.chars()
            .enumerate()
            .flat_map(|(i, c)| {
                if i != 0 && i % 5 == 0 {
                    vec!['-', c]
                } else {
                    vec![c]
                }
            })
            .collect()
    }

    #[test]
    fn workflow_step_occurrence_is_surfaced_workflow() {
        let c = classify(&id_of("wa", 30));
        assert!(c.recognized);
        assert_eq!(c.family, "workflow");
        assert_eq!(c.type_code.as_deref(), Some("wa"));
        assert_eq!(c.entity_type, "WorkflowStepOccurrence");
        assert_eq!(c.surfacing, Some(Surfacing::Surfaced));
        assert_eq!(c.length, 30);
    }

    #[test]
    fn storage_node_is_surfaced_non_workflow() {
        let c = classify(&id_of("2", 29));
        assert!(c.recognized);
        assert_eq!(c.family, "non-workflow");
        assert_eq!(c.type_code.as_deref(), Some("2"));
        assert_eq!(c.entity_type, "StorageNode");
        assert_eq!(c.surfacing, Some(Surfacing::Surfaced));
        assert_eq!(c.length, 29);
    }

    #[test]
    fn every_workflow_code_maps() {
        for (code, entity) in [
            ("wa", "WorkflowStepOccurrence"),
            ("wd", "WorkflowTrigger"),
            ("wf", "WorkflowTemplate"),
            ("we", "WorkflowObligation"),
            ("wt", "WorkflowReview"),
            ("wq", "WorkspacePolicy"),
            ("wp", "WorkflowRole"),
            ("wm", "WorkflowOutboundSub"),
            ("wb", "WorkflowEdge"),
            ("wc", "WorkflowLoop"),
            ("wg", "WorkflowTransformHandler"),
            ("wh", "WorkflowOverlay"),
            ("wj", "WorkflowCheckpoint"),
            ("wk", "WorkflowEvidenceSnapshot"),
            ("wn", "WorkflowInboundNonce"),
            ("wr", "WorkflowNlLedger"),
            ("ws", "WorkflowPool"),
        ] {
            let c = classify(&id_of(code, 30));
            assert!(c.recognized, "{code} should be recognized");
            assert_eq!(c.family, "workflow", "{code}");
            assert_eq!(c.entity_type, entity, "{code}");
        }
    }

    #[test]
    fn every_non_workflow_code_maps() {
        for (code, entity) in [
            ("2", "StorageNode"),
            ("3", "StorageVersion"),
            ("5", "Upload"),
            ("6", "Share"),
            ("8", "Asset"),
            ("9", "AiJob"),
            ("e", "OrgTransfer"),
            ("c", "Metadata"),
            ("4", "StoragePhy"),
            ("7", "StorageData"),
            ("f", "ChunkManifest"),
            ("b", "AiTransaction"),
            ("d", "AIAgentKey"),
            ("a", "General"),
        ] {
            let c = classify(&id_of(code, 29));
            assert!(c.recognized, "{code} should be recognized");
            assert_eq!(c.family, "non-workflow", "{code}");
            assert_eq!(c.entity_type, entity, "{code}");
        }
    }

    #[test]
    fn hyphenated_forms_classify_identically_after_stripping() {
        // 30-char workflow id, hyphenated to 35 chars.
        let raw = id_of("wa", 30);
        let formatted = hyphenate(&raw);
        assert_eq!(formatted.len(), 35);
        let c = classify(&formatted);
        assert!(c.recognized);
        assert_eq!(c.entity_type, "WorkflowStepOccurrence");
        assert_eq!(c.raw, raw);
        assert_eq!(c.length, 30);

        // 29-char node id, hyphenated to 34 chars.
        let raw29 = id_of("2", 29);
        let formatted29 = hyphenate(&raw29);
        assert_eq!(formatted29.len(), 34);
        let c29 = classify(&formatted29);
        assert!(c29.recognized);
        assert_eq!(c29.entity_type, "StorageNode");
        assert_eq!(c29.length, 29);
    }

    #[test]
    fn classification_is_case_insensitive_but_preserves_input() {
        let raw = id_of("WA", 30); // upper-case prefix
        let c = classify(&raw);
        assert!(c.recognized);
        assert_eq!(c.entity_type, "WorkflowStepOccurrence");
        assert_eq!(c.type_code.as_deref(), Some("wa")); // lowercased code
        assert_eq!(c.input, raw); // input echoed verbatim (not lower-cased)
    }

    #[test]
    fn transitional_single_char_code_is_not_classified_workflow() {
        // §4: a 29-char id whose 1-char code is a transitional workflow letter
        // (e.g. `g`) must be reported unknown, NEVER guessed as workflow.
        let c = classify(&id_of("g", 29));
        assert!(!c.recognized);
        assert_eq!(c.family, "unknown");
        assert_ne!(c.family, "workflow");
        assert_eq!(c.entity_type, "Unknown");
        assert!(c.note.contains("transitional"));
    }

    #[test]
    fn length_30_not_leading_w_is_unknown() {
        let c = classify(&id_of("xa", 30));
        assert!(!c.recognized);
        assert_eq!(c.family, "unknown");
        assert!(c.note.contains("does not lead with 'w'"));
    }

    #[test]
    fn length_30_unmapped_workflow_subtype_is_unrecognized_workflowish() {
        let c = classify(&id_of("wz", 30)); // `wz` is not in the map
        assert!(!c.recognized);
        assert_eq!(c.family, "unknown");
        assert_eq!(c.type_code.as_deref(), Some("wz"));
        assert!(c.note.contains("workflow subtype"));
    }

    #[test]
    fn sentinel_zero_is_recognized_false() {
        let c = classify(&id_of("0", 29));
        assert!(!c.recognized);
        assert_eq!(c.family, "non-workflow");
        assert_eq!(c.entity_type, "Unknown");
        assert!(c.note.contains("sentinel"));
    }

    #[test]
    fn nineteen_digit_numeric_is_flagged_as_profile_id() {
        let c = classify("3867689418901071163");
        assert!(!c.recognized);
        assert_eq!(c.family, "unknown");
        assert_eq!(c.type_code, None);
        assert!(c.note.contains("19-digit numeric profile id"));
    }

    #[test]
    fn garbage_and_empty_are_unknown() {
        for junk in ["", "   ", "not-an-id", "hello world", "https://x/y"] {
            let c = classify(junk);
            assert!(!c.recognized, "{junk:?} should be unrecognized");
            assert_eq!(c.family, "unknown", "{junk:?}");
            assert_eq!(c.type_code, None, "{junk:?}");
        }
    }

    #[test]
    fn outer_whitespace_is_trimmed() {
        let raw = id_of("2", 29);
        let padded = format!("  {raw}\n");
        let c = classify(&padded);
        assert!(c.recognized);
        assert_eq!(c.input, raw);
        assert_eq!(c.entity_type, "StorageNode");
    }

    #[test]
    fn to_json_shape_and_surfacing_token() {
        let c = classify(&id_of("wt", 30));
        let v = to_json(&c);
        assert_eq!(v["entity_type"], "WorkflowReview");
        assert_eq!(v["family"], "workflow");
        assert_eq!(v["surfacing"], "surfaced");
        assert_eq!(v["recognized"], true);
        assert_eq!(v["length"], 30);
        // An unrecognized input serializes surfacing as null.
        let u = to_json(&classify("garbage"));
        assert!(u["surfacing"].is_null());
        assert!(u["type_code"].is_null());
    }
}
