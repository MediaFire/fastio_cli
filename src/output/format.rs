/// Field filtering and format selection utilities.
///
/// Provides the logic for `--fields` filtering across all output formats.
use serde_json::Value;

/// Filter a JSON value so only the requested fields survive on each **record**.
///
/// If `fields` is `None` (or empty), the value is returned unchanged.
///
/// # Why this projects records rather than top-level keys
///
/// Fast.io responses arrive here already unwrapped from the transport envelope
/// (`client.rs` strips `{result, response}`), but the payload is still wrapped
/// in a **named resource key**: `{"workspaces": [ … ]}`, `{"org": { … }}`. A
/// naive top-level filter therefore matched nothing for the field names a user
/// actually types — `--fields name,id` looked for `name`/`id` as siblings of
/// `workspaces` and returned `{}` — while `--fields workspaces` "worked" by
/// returning the whole array **completely unfiltered**. Both failures were
/// silent, with exit code 0.
///
/// So the projection is **structure-preserving**: the wrapper is kept and the
/// records inside it are filtered. That keeps every format correct from one
/// implementation —
///
/// * `--format json` keeps the envelope the caller expects,
/// * `--format markdown` keeps the preamble/section envelope its server
///   contract requires (it must never go through `flatten_response`),
/// * `--format table|csv` still finds the projected array via
///   [`crate::output::flatten_response`], which prefers non-empty arrays.
///
/// # Record vs envelope
///
/// Each object is classified, then walked PER KEY. An earlier version decided
/// the whole object with a handful of booleans and returned early; that shape
/// cannot express the cases below, each of which it got wrong in a different
/// way.
///
/// 1. An **array element** is always a record — projected directly.
/// 2. An object with **no container children** is a bare record — projected
///    directly, *even when it matches nothing*, so an unknown field yields `{}`
///    rather than the whole object.
/// 3. An object where **every requested field is present here** is the record
///    the caller meant — projected, not descended into. Without this a detail
///    body leaked its other scalars: `--fields name` on a share also returned
///    `id` and `secret_token`.
/// 4. Otherwise walk key by key: a **requested key** is taken whole (container
///    or scalar); any **container** is projected, so a requested field in a
///    sibling is never suppressed and nested arrays are still traversed; an
///    **unrequested scalar** survives only on a TRUE envelope.
///
/// A "true envelope" is an object that matches nothing here but does carry a
/// requested field **deeper** ([`subtree_has_match`]) — that is what keeps
/// `result` / counts / cursors alongside the records. An object matching
/// nothing *anywhere* is not an envelope, and copying its scalars is how
/// `--fields nonexistent` used to return a record's `secret_token`.
///
/// A requested field that matches nothing still yields an empty object, so a
/// typo never silently returns unfiltered data — at every level.
///
#[must_use]
pub fn filter_fields(value: &Value, fields: Option<&[String]>) -> Value {
    let Some(field_list) = fields else {
        return value.clone();
    };

    if field_list.is_empty() {
        return value.clone();
    }

    project(value, field_list)
}

/// Does any object anywhere in `value` carry one of `fields`?
///
/// Structurally, an ENVELOPE that owns records and a RECORD that owns a
/// collection are the same shape once neither matches at the top level — and
/// they need opposite treatment. An envelope must keep its scalar siblings
/// (`result`, counts, cursors); a record must not, or `--fields nonexistent`
/// hands back `id`, `name` and `secret_token` untouched.
///
/// The one signal that separates them is whether the requested field exists
/// DEEPER. If it does, this object is carrying records and is an envelope; if
/// it exists nowhere, nothing here was asked for and unrequested scalars must
/// not ride along. (`an_unknown_field_does_not_leak_a_records_scalars`)
fn subtree_has_match(value: &Value, fields: &[String]) -> bool {
    match value {
        Value::Object(map) => {
            fields.iter().any(|f| map.contains_key(f))
                || map.values().any(|v| subtree_has_match(v, fields))
        }
        Value::Array(items) => items.iter().any(|v| subtree_has_match(v, fields)),
        _ => false,
    }
}

/// Recursively project `fields` onto every record reachable from `value`,
/// preserving the surrounding structure. See [`filter_fields`].
fn project(value: &Value, fields: &[String]) -> Value {
    match value {
        // Every OBJECT element of an array is a record, unconditionally — it is
        // projected even when it carries none of the requested fields, so an
        // unknown field yields `{}` rather than falling through to the wrapper
        // branch and passing the whole row back unfiltered. (Caught by
        // `unknown_field_yields_empty_records_not_passthrough`.)
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| match item {
                    Value::Object(_) => filter_object(item, fields),
                    nested => project(nested, fields),
                })
                .collect(),
        ),
        Value::Object(map) => {
            // A BARE record — no container children at all — is a record even
            // when it matches nothing, so an unknown field yields `{}` rather
            // than the whole object. (`a_bare_record_with_an_unknown_field_…`)
            let holds_any_container = map.values().any(|v| v.is_array() || v.is_object());
            if !holds_any_container {
                return filter_object(value, fields);
            }
            // If EVERY requested field is present right here, this object IS the
            // record the caller meant: project it and stop, even though it owns
            // a container. Without this a detail body leaked its other scalars —
            // `--fields name` on a share returned `id` and `secret_token`.
            // (`a_record_owning_a_container_does_not_leak_its_other_scalars`)
            if fields.iter().all(|field| map.contains_key(field)) {
                return filter_object(value, fields);
            }
            // Otherwise walk PER KEY. An earlier version decided the whole
            // object with a few booleans and returned early, which was wrong in
            // two ways an object-wide verdict cannot express:
            //   * naming one container key suppressed descent into its SIBLINGS,
            //     so `--fields actions,status,answer` dropped `answer`;
            //   * arrays-of-arrays were not "record arrays", so `--fields
            //     name,count` on `{"rows":[[{…}]],"count":1}` dropped `rows`.
            // Per-key traversal has no such blind spot and needs no
            // classification scans. (`a_named_container_does_not_suppress_…`,
            // `nested_arrays_are_traversed`)
            let matches_here = fields.iter().any(|field| map.contains_key(field));
            let subtree_matches = subtree_has_match(value, fields);
            let mut out = serde_json::Map::new();
            for (key, val) in map {
                if fields.iter().any(|f| f == key) {
                    // Explicitly requested: take it whole, container or scalar.
                    out.insert(key.clone(), val.clone());
                } else if val.is_array() || val.is_object() {
                    // Not requested, but a requested field may live inside.
                    out.insert(key.clone(), project(val, fields));
                } else if !matches_here && subtree_matches {
                    // An unrequested scalar survives only on a TRUE envelope —
                    // one that matched nothing here but DOES carry the requested
                    // field deeper, so `result` / counts / cursors must reach
                    // the renderer alongside the records.
                    //
                    // On a record that matched something, the caller named the
                    // fields they wanted. And on an object that matches nothing
                    // anywhere, there is no envelope to preserve — copying its
                    // scalars is how `--fields nonexistent` returned a whole
                    // record including its `secret_token`.
                    out.insert(key.clone(), val.clone());
                }
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Filter a single JSON object to include only the specified keys.
fn filter_object(value: &Value, fields: &[String]) -> Value {
    let Value::Object(map) = value else {
        return value.clone();
    };

    let mut filtered = serde_json::Map::new();
    for field in fields {
        if let Some(v) = map.get(field) {
            filtered.insert(field.clone(), v.clone());
        }
    }
    Value::Object(filtered)
}

#[cfg(test)]
mod tests {
    use super::{filter_fields, project};
    use serde_json::{Value, json};

    fn fields(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn none_or_empty_returns_value_unchanged() {
        let v = json!({"workspaces": [{"id": 1, "name": "a"}]});
        assert_eq!(filter_fields(&v, None), v);
        assert_eq!(filter_fields(&v, Some(&[])), v);
    }

    /// THE REGRESSION THIS FIX EXISTS FOR.
    ///
    /// `{"workspaces": [...]}` is the API's standard listing shape. Filtering
    /// the WRAPPER's keys meant `--fields name,id` matched nothing and rendered
    /// `{}` — an empty result with exit code 0, indistinguishable from "no
    /// matching records". Pinned so the ordering cannot regress.
    #[test]
    fn projects_records_inside_a_named_wrapper_not_the_wrapper_itself() {
        let v = json!({
            "workspaces": [
                {"id": 1, "name": "alpha", "folder_name": "f1"},
                {"id": 2, "name": "beta", "folder_name": "f2"}
            ]
        });
        let out = filter_fields(&v, Some(&fields(&["name", "id"])));
        assert_eq!(
            out,
            json!({"workspaces": [{"id": 1, "name": "alpha"}, {"id": 2, "name": "beta"}]}),
            "records must be projected in place, wrapper preserved"
        );
    }

    /// The single-resource shape (`/org/{id}/details/`) must project too.
    #[test]
    fn projects_a_single_wrapped_object() {
        let v = json!({"result": true, "org": {"id": 7, "name": "acme", "domain": "x.io"}});
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(out, json!({"result": true, "org": {"name": "acme"}}));
    }

    /// Scalar envelope siblings survive — markdown renders the full envelope
    /// and must not lose its preamble, and JSON callers expect `result`.
    #[test]
    fn scalar_envelope_siblings_are_preserved() {
        let v = json!({"result": true, "count": 2, "workspaces": [{"id": 1, "name": "a"}]});
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(out["result"], json!(true));
        assert_eq!(out["count"], json!(2));
        assert_eq!(out["workspaces"], json!([{"name": "a"}]));
    }

    /// A bare array (no wrapper) keeps the original element-wise behaviour.
    #[test]
    fn bare_array_is_projected_element_wise() {
        let v = json!([{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]);
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(out, json!([{"name": "a"}, {"name": "b"}]));
    }

    /// An unknown field yields an EMPTY record, never unfiltered data. This is
    /// the property that makes a typo visibly empty instead of silently
    /// passing the whole row through.
    #[test]
    fn unknown_field_yields_empty_records_not_passthrough() {
        let v = json!({"workspaces": [{"id": 1, "name": "a"}]});
        let out = filter_fields(&v, Some(&fields(&["nonexistent_column"])));
        assert_eq!(out, json!({"workspaces": [{}]}));
    }

    /// Asking for the WRAPPER key is treated as a record match on the envelope
    /// — it returns that key, and deliberately does not also project inside it.
    #[test]
    fn requesting_the_wrapper_key_returns_that_key() {
        let v = json!({"result": true, "workspaces": [{"id": 1, "name": "a"}]});
        let out = filter_fields(&v, Some(&fields(&["workspaces"])));
        assert_eq!(out, json!({"workspaces": [{"id": 1, "name": "a"}]}));
    }

    /// Once an object matches, its nested children are NOT descended into, so a
    /// child reusing the same field name cannot overwrite the outer match.
    #[test]
    fn matching_object_is_not_descended_into() {
        let v = json!({"org": {"name": "outer", "settings": {"name": "inner"}}});
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(out, json!({"org": {"name": "outer"}}));
    }

    /// Unified-search `buckets` keep their structure so `render_buckets` still
    /// sees one section per bucket; only the items are projected.
    #[test]
    fn bucket_structure_survives_projection() {
        let v = json!({
            "buckets": {
                "files": {"items": [{"id": 1, "name": "a"}], "total": 1},
                "comments": {"items": [{"id": 9, "name": "c"}], "total": 1}
            }
        });
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(out["buckets"]["files"]["items"], json!([{"name": "a"}]));
        assert_eq!(out["buckets"]["comments"]["items"], json!([{"name": "c"}]));
        assert_eq!(
            out["buckets"]["files"]["total"],
            json!(1),
            "bucket metadata must survive"
        );
    }

    /// Naming one container key suppressed descent into its SIBLINGS, so a
    /// field the user explicitly requested was dropped.
    /// `--fields actions,status,answer` returned `actions` and `status` and
    /// silently lost `answer`, which lives in a sibling container.
    #[test]
    fn a_named_container_does_not_suppress_traversal_of_its_siblings() {
        let v = json!({"message": {
            "status": "complete",
            "actions": [{"kind": "x"}],
            "result": {"answer": "A"}
        }});
        let out = filter_fields(&v, Some(&fields(&["actions", "status", "answer"])));
        assert_eq!(
            out["message"]["result"]["answer"],
            json!("A"),
            "a requested field in a sibling container must survive, got: {out}"
        );
        assert_eq!(out["message"]["actions"], json!([{"kind": "x"}]));
        assert_eq!(out["message"]["status"], json!("complete"));
    }

    /// An array of arrays is still a path to records. The
    /// classifier only recognised arrays whose elements were objects, so
    /// `{"rows":[[{…}]],"count":1}` with `--fields name,count` dropped `rows`.
    #[test]
    fn nested_arrays_are_traversed() {
        let v = json!({"rows": [[{"name": "a", "id": 1}]], "count": 1});
        assert_eq!(
            filter_fields(&v, Some(&fields(&["name", "count"]))),
            json!({"rows": [[{"name": "a"}]], "count": 1}),
            "records nested two array levels deep must still be projected"
        );
    }

    /// THE LEAK. `--fields name` returned four fields, one of them a token.
    ///
    /// A single-resource detail body owns a nested collection, so it escaped the
    /// bare-record rule, fell to the wrapper branch, and that branch copied
    /// EVERY scalar — handing back `id` and `secret_token` for a one-field
    /// request. Found by the Opus review team, reproduced by execution before
    /// fixing.
    ///
    /// No fixture anywhere put a nested object array *inside* a record, which is
    /// exactly the shape the classifier keys on — every other one is a flat
    /// record or a wrapper of flat records.
    #[test]
    fn a_record_owning_a_container_does_not_leak_its_other_scalars() {
        let v = json!({
            "result": true,
            "share": {
                "id": 7, "name": "acme", "secret_token": "tok",
                "recipients": [{"email": "a@b.c"}]
            }
        });
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(
            out,
            json!({"result": true, "share": {"name": "acme"}}),
            "only the requested field may come back from the record"
        );
        let share = &out["share"];
        assert!(share.get("secret_token").is_none(), "must not leak a token");
        assert!(
            share.get("id").is_none(),
            "must not leak unrequested scalars"
        );

        // …and the same body with an EMPTY collection answers identically.
        let empty = json!({
            "result": true,
            "share": {"id": 7, "name": "acme", "secret_token": "tok", "recipients": []}
        });
        assert_eq!(
            filter_fields(&empty, Some(&fields(&["name"]))),
            out,
            "row count must not change the answer"
        );
    }

    /// When a requested field lives DEEPER, descending is right — but the
    /// collision record still must not copy its unrequested scalars.
    #[test]
    fn descending_for_a_deeper_field_still_does_not_leak() {
        let v = json!({
            "share": {
                "id": 7, "name": "acme", "secret_token": "tok",
                "recipients": [{"email": "a@b.c", "role": "viewer"}]
            }
        });
        let out = filter_fields(&v, Some(&fields(&["name", "email"])));
        assert_eq!(
            out,
            json!({"share": {"name": "acme", "recipients": [{"email": "a@b.c"}]}}),
            "descend for `email`, keep `name`, drop everything unasked-for"
        );
    }

    /// An EMPTY result set must not change the output's SHAPE.
    ///
    /// Found by executing the edge cases rather than reasoning about them:
    /// `--fields count` on `{"workspaces": [], "count": 2}` dropped the empty
    /// array, while the identical request against a populated result kept it.
    /// So whether a key exists in `--format json` depended on whether the
    /// result happened to have rows — a data-dependent shape change, which is
    /// exactly what breaks a consuming script.
    #[test]
    fn an_empty_record_array_keeps_the_same_shape_as_a_populated_one() {
        let empty = json!({"workspaces": [], "count": 2});
        let populated = json!({"workspaces": [{"id": 1, "name": "x"}], "count": 2});
        let f = fields(&["count"]);

        let out_empty = filter_fields(&empty, Some(&f));
        let out_populated = filter_fields(&populated, Some(&f));

        // `count` is the only requested field and it lives right here, so this
        // object IS the record: the answer is `{"count": 2}` either way. (An
        // earlier version of this test asserted `workspaces` must survive —
        // that was wrong. Asking for one field should return one field; the
        // property worth pinning is that the row count cannot change the
        // answer's SHAPE.)
        assert_eq!(out_empty, json!({"count": 2}));
        assert_eq!(out_populated, json!({"count": 2}));
        // The shapes must agree on which KEYS exist, whatever the row count.
        let keys = |v: &Value| {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(
            keys(&out_empty),
            keys(&out_populated),
            "an empty result must not change which keys are present"
        );
    }

    /// …but an array of SCALARS is not a record array, so a match still stops.
    #[test]
    fn a_scalar_array_does_not_make_an_object_a_wrapper() {
        let v = json!({"tags": ["a", "b"], "name": "x"});
        assert_eq!(
            filter_fields(&v, Some(&fields(&["name"]))),
            json!({"name": "x"}),
            "a string array must not turn a record into a wrapper"
        );
    }

    /// The second half of the heuristic bug.
    ///
    /// A bare record matching nothing fell through to the wrapper branch, whose
    /// scalar passthrough returned it **completely unfiltered** — the exact
    /// opposite of the documented guarantee, and the same silent-wrong shape as
    /// the pre-rewrite bug. `unknown_field_yields_empty_records_not_passthrough`
    /// missed it because its fixture is WRAPPED, and the array branch forces
    /// `filter_object` before the wrapper heuristic is ever consulted.
    #[test]
    fn a_bare_record_with_an_unknown_field_is_emptied_not_passed_through() {
        let v = json!({"id": 1, "name": "a"});
        assert_eq!(
            filter_fields(&v, Some(&fields(&["nonexistent_column"]))),
            json!({}),
            "a bare record must be emptied by an unknown field, never passed through"
        );
        // …and a known field still projects normally.
        assert_eq!(
            filter_fields(&v, Some(&fields(&["name"]))),
            json!({"name": "a"})
        );
    }

    /// THE COLLISION THE WRAPPER/RECORD HEURISTIC ORIGINALLY MISSED.
    ///
    /// `--fields name,count` on the standard listing shape matches `count` on
    /// the ENVELOPE. Treating "any requested key present" as "this is the
    /// record" meant projecting the wrapper and returning `{"count": 2}` —
    /// **dropping the records entirely, with exit 0**. That is the same
    /// silent-wrong failure this projection exists to remove, reintroduced one
    /// level up, and it reached every command because `--fields` is global.
    ///
    /// The tests that shipped with the rewrite never mixed an envelope key with
    /// a record key, which is exactly why they missed it.
    #[test]
    fn envelope_key_colliding_with_a_requested_field_does_not_drop_records() {
        let v = json!({
            "workspaces": [{"id": 1, "name": "alpha"}, {"id": 2, "name": "beta"}],
            "count": 2
        });
        let out = filter_fields(&v, Some(&fields(&["name", "count"])));
        assert_eq!(
            out["workspaces"],
            json!([{"name": "alpha"}, {"name": "beta"}]),
            "records must survive when a requested field also exists on the envelope"
        );
        assert_eq!(
            out["count"],
            json!(2),
            "the matched envelope key is kept too"
        );
    }

    /// The same collision via the other envelope scalars a caller might name.
    #[test]
    fn other_envelope_scalars_also_do_not_swallow_the_records() {
        let v = json!({
            "result": true,
            "has_more": false,
            "workspaces": [{"id": 1, "name": "a"}]
        });
        for requested in [
            vec!["name", "result"],
            vec!["id", "has_more"],
            vec!["name", "result", "has_more"],
        ] {
            let out = filter_fields(&v, Some(&fields(&requested)));
            assert!(
                out["workspaces"].as_array().is_some_and(|a| !a.is_empty()),
                "requesting {requested:?} must not drop the records, got: {out}"
            );
        }
    }

    /// The guard must NOT fire for a nested plain object — that is the case the
    /// stop-on-match rule was written for, and it still holds.
    #[test]
    fn a_matching_object_with_no_record_array_still_stops_descending() {
        let v = json!({"org": {"name": "outer", "settings": {"name": "inner"}}});
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(
            out,
            json!({"org": {"name": "outer"}}),
            "a nested object reusing a field name must not override the outer match"
        );
    }

    /// Empty containers survive; unrequested scalars do NOT when the field
    /// matches nowhere.
    ///
    /// This test asserted full passthrough — `{"workspaces": [], "note": "hi"}`
    /// unchanged — which is the behaviour that let `--fields nonexistent` hand
    /// back a record's `secret_token`. An unrequested scalar rides along only on
    /// a TRUE envelope, i.e. one that carries the requested field deeper. Here
    /// `name` exists nowhere, so there is no envelope to preserve and `note` is
    /// not something the caller asked for.
    ///
    /// The empty array still survives, because a container is structure rather
    /// than data — dropping it would make the output shape depend on the row
    /// count, which is separately pinned.
    #[test]
    fn empty_containers_survive_but_unrequested_scalars_do_not() {
        let v = json!({"workspaces": [], "note": "hi"});
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(out, json!({"workspaces": []}));

        assert_eq!(project(&json!("plain"), &fields(&["name"])), json!("plain"));
        assert_eq!(project(&Value::Null, &fields(&["name"])), Value::Null);
    }

    /// A DELIBERATE, AMBIGUOUS CASE — pinned so it is a decision, not an accident.
    ///
    /// When a record field shares a name with an envelope key AND it is the only
    /// requested field, the envelope wins and the rows drop:
    ///
    /// ```text
    /// {"count":2,"nodes":[{"name":"f","count":10}]}  --fields count  ->  {"count":2}
    /// ```
    ///
    /// The intent is genuinely ambiguous — the caller may have meant per-node
    /// counts. The rule is "if every requested field is present here, this is the
    /// record", which answers the request as literally asked. Adding any second
    /// non-envelope field restores the rows, because then not everything is
    /// satisfied at this level.
    ///
    /// Raised by the review team, who checked first that it followed from a rule
    /// I had already chosen deliberately. It was the one case the collision test
    /// did not reach (that one uses two fields, so `all` is false there).
    #[test]
    fn a_single_field_colliding_with_an_envelope_key_resolves_to_the_envelope() {
        let v = json!({"count": 2, "nodes": [{"name": "f", "count": 10}]});
        assert_eq!(
            filter_fields(&v, Some(&fields(&["count"]))),
            json!({"count": 2}),
            "one requested field, present here ⇒ this object is the record"
        );
        // A second field that is NOT on the envelope restores the rows.
        assert_eq!(
            filter_fields(&v, Some(&fields(&["count", "name"]))),
            json!({"count": 2, "nodes": [{"name": "f", "count": 10}]}),
            "adding a record-only field makes this an envelope again"
        );
    }

    /// An unknown `--fields` on a record that owns a collection
    /// returned the ENTIRE record — `id`, `name` and `secret_token` — because a
    /// record-with-container and an envelope-with-container are structurally
    /// identical once neither matches locally. The subtree probe is what tells
    /// them apart.
    #[test]
    fn an_unknown_field_does_not_leak_a_records_scalars() {
        let v = json!({"id": 7, "name": "a", "secret_token": "s", "tags": [{"x": 1}]});
        let out = filter_fields(&v, Some(&fields(&["nonexistent_column"])));
        assert!(
            out.get("secret_token").is_none(),
            "an unmatched field must not return a secret, got: {out}"
        );
        assert!(out.get("id").is_none(), "nor any other unrequested scalar");
        assert_eq!(out, json!({"tags": [{}]}));
    }

    /// …but a TRUE envelope still keeps its siblings, because the requested
    /// field really is carried deeper. This is the case the leak fix must not
    /// break.
    #[test]
    fn a_true_envelope_still_keeps_its_scalar_siblings() {
        let v = json!({"result": true, "count": 2, "workspaces": [{"id": 1, "name": "a"}]});
        let out = filter_fields(&v, Some(&fields(&["name"])));
        assert_eq!(out["result"], json!(true));
        assert_eq!(out["count"], json!(2));
        assert_eq!(out["workspaces"], json!([{"name": "a"}]));
    }
}
