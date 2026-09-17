//! `build_forest` / `picker_order`.
//!
//! Pure functions over `SessionEntry`, so these run without a server, a client, or a
//! terminal. Both the sidebar and the `/tree` overlay consume the output, so the
//! invariant that matters most is stated first: no session may ever be lost.

use kn9t_tui::session_manager::SessionEntry;
use kn9t_tui::session_tree::{build_forest, picker_order, reason_badge};

/// `id` with an optional parent. `head_seq` and dates are irrelevant to shape.
fn e(id: &str, parent: Option<&str>) -> SessionEntry {
    SessionEntry {
        id: id.into(),
        name: id.into(),
        parent_id: parent.map(|p| p.to_string()),
        ..Default::default()
    }
}

fn with_reason(id: &str, parent: &str, reason: &str) -> SessionEntry {
    SessionEntry {
        fork_reason: Some(reason.into()),
        ..e(id, Some(parent))
    }
}

/// Depth-first order, and the (idx, depth) pairs, as a readable assertion target.
fn shape(entries: &[SessionEntry]) -> Vec<(String, usize)> {
    build_forest(entries)
        .flatten()
        .into_iter()
        .map(|n| (entries[n.idx].id.clone(), n.depth))
        .collect()
}

#[test]
fn empty_list_is_an_empty_forest() {
    let f = build_forest(&[]);
    assert!(f.is_empty());
    assert_eq!(f.len(), 0);
}

#[test]
fn one_root_with_three_direct_children() {
    let entries = vec![
        e("root", None),
        e("a", Some("root")),
        e("b", Some("root")),
        e("c", Some("root")),
    ];
    assert_eq!(
        shape(&entries),
        vec![
            ("root".into(), 0),
            ("a".into(), 1),
            ("b".into(), 1),
            ("c".into(), 1),
        ]
    );
}

/// A rewind on a fork on a fork — the case `/undo` after `/fork` produces.
#[test]
fn deep_chain_keeps_increasing_depth() {
    let entries = vec![
        e("root", None),
        with_reason("f1", "root", "fork"),
        with_reason("f2", "f1", "fork"),
        with_reason("r1", "f2", "rewind"),
    ];
    assert_eq!(
        shape(&entries),
        vec![
            ("root".into(), 0),
            ("f1".into(), 1),
            ("f2".into(), 2),
            ("r1".into(), 3),
        ]
    );
}

#[test]
fn several_roots_form_a_forest_in_input_order() {
    let entries = vec![
        e("r1", None),
        e("r2", None),
        e("r1a", Some("r1")),
        e("r3", None),
    ];
    let f = build_forest(&entries);
    assert_eq!(f.roots.len(), 3, "three parentless sessions");
    assert_eq!(
        shape(&entries),
        vec![
            ("r1".into(), 0),
            ("r1a".into(), 1),
            ("r2".into(), 0),
            ("r3".into(), 0),
        ]
    );
}

/// Children are emitted directly under their parent even when the flat list interleaves
/// them — this is the whole point of the sidebar change, and the reason the renderer
/// cannot just iterate the `Vec` in order.
#[test]
fn child_follows_its_parent_regardless_of_list_position() {
    let entries = vec![
        e("child", Some("parent")),
        e("unrelated", None),
        e("parent", None),
    ];
    assert_eq!(
        shape(&entries),
        vec![
            ("unrelated".into(), 0),
            ("parent".into(), 0),
            ("child".into(), 1),
        ]
    );
}

/// A child whose parent is absent (deleted, or filtered out by the picker's search) is
/// promoted to a root. Indexing strictly by parent would silently drop it, which would
/// make the search box hide sessions that match it.
#[test]
fn orphan_is_promoted_to_root_not_dropped() {
    let entries = vec![e("orphan", Some("gone")), e("root", None)];
    let f = build_forest(&entries);
    assert_eq!(f.len(), 2, "no entry may be lost");
    assert_eq!(
        shape(&entries),
        vec![("orphan".into(), 0), ("root".into(), 0)]
    );
}

/// `origin_session` always points at an older row, so this cannot arise from the store.
/// Asserted anyway: the guarantee callers rely on is "every entry appears exactly once",
/// and a cycle must not turn that into a hang or a lost session.
#[test]
fn cycles_do_not_hang_and_lose_nothing() {
    let entries = vec![e("a", Some("b")), e("b", Some("a"))];
    let f = build_forest(&entries);
    assert_eq!(f.len(), 2);
    let ids: Vec<String> = shape(&entries).into_iter().map(|(id, _)| id).collect();
    assert!(ids.contains(&"a".to_string()) && ids.contains(&"b".to_string()));
}

#[test]
fn self_parent_is_treated_as_a_root() {
    let entries = vec![e("loop", Some("loop"))];
    assert_eq!(shape(&entries), vec![("loop".into(), 0)]);
}

// ── picker_order: the renderer/key-handler contract ────────────────

#[test]
fn picker_order_covers_every_entry_and_indexes_the_full_list() {
    let entries = vec![
        e("root", None),
        e("a", Some("root")),
        e("other", None),
    ];
    let order = picker_order(&entries, "");
    assert_eq!(order.len(), entries.len());
    // Indices address the ORIGINAL slice, since the key handler resolves them against
    // `self.session.sessions`, not against the filtered subset.
    let ids: Vec<&str> = order.iter().map(|&(i, _)| entries[i].id.as_str()).collect();
    assert_eq!(ids, vec!["root", "a", "other"]);
    assert_eq!(order[1].1, 1, "the child is indented");
}

/// The filter runs first, then the tree is built over what survives. A child that matches
/// while its parent does not stays visible, as a root.
#[test]
fn picker_order_promotes_a_match_whose_parent_was_filtered_out() {
    let entries = vec![e("alpha", None), e("beta", Some("alpha"))];
    let order = picker_order(&entries, "beta");
    assert_eq!(order.len(), 1);
    assert_eq!(entries[order[0].0].id, "beta");
    assert_eq!(order[0].1, 0, "promoted to root, so not indented");
}

#[test]
fn picker_order_with_no_matches_is_empty() {
    let entries = vec![e("alpha", None)];
    assert!(picker_order(&entries, "zzz").is_empty());
}

#[test]
fn reason_badge_distinguishes_the_four_reasons_and_ignores_unknown() {
    let all = ["fork", "rewind", "subagent", "tree"].map(|r| reason_badge(Some(r)));
    assert_eq!(
        all.iter().collect::<std::collections::HashSet<_>>().len(),
        4,
        "each reason must be visually distinct"
    );
    assert_eq!(reason_badge(None), "", "a root carries no badge");
    assert_eq!(reason_badge(Some("weird")), "");
}

// ── /fork and /undo argument rules ─────────────────────────────────

use kn9t_tui::session_tree::plan_fork;

/// The default: undo one message. `head - 1` is where the branch starts, because the log is
/// append-only — dropping a message means branching before it, never editing in place.
#[test]
fn undo_without_an_argument_drops_one_message() {
    let p = plan_fork("undo", "", 5).unwrap();
    assert_eq!(p.reason, "rewind");
    assert_eq!(p.origin_seq, Some(4));
    assert_eq!(p.note, "1 message undone.");
}

#[test]
fn undo_n_drops_n_messages() {
    let p = plan_fork("undo", "3", 10).unwrap();
    assert_eq!(p.origin_seq, Some(7));
    assert_eq!(p.note, "3 messages undone.", "plural reads correctly");
}

/// Undoing exactly as many messages as exist is legal and lands on an empty branch.
#[test]
fn undo_everything_lands_at_seq_zero() {
    let p = plan_fork("undo", "4", 4).unwrap();
    assert_eq!(p.origin_seq, Some(0));
}

/// A fresh session has nothing to undo. The user is told, rather than getting an empty
/// branch that looks like a bug.
#[test]
fn undo_past_the_beginning_is_refused() {
    let err = plan_fork("undo", "", 0).unwrap_err();
    assert!(err.contains("Nothing to undo"), "got: {err}");
    assert!(plan_fork("undo", "5", 3).is_err(), "more than exists");
}

#[test]
fn undo_rejects_zero_and_garbage() {
    assert!(plan_fork("undo", "0", 5).is_err(), "zero is not a step back");
    assert!(plan_fork("undo", "abc", 5).is_err());
    assert!(plan_fork("undo", "-2", 5).is_err(), "negative is not a count");
}

/// `/fork` with no argument checkpoints at the current head: same history, new branch.
#[test]
fn fork_without_an_argument_branches_at_head() {
    let p = plan_fork("fork", "", 7).unwrap();
    assert_eq!(p.reason, "fork");
    assert_eq!(p.origin_seq, Some(7));
}

#[test]
fn fork_accepts_an_explicit_earlier_seq() {
    let p = plan_fork("fork", "2", 7).unwrap();
    assert_eq!(p.origin_seq, Some(2));
    assert!(p.note.contains('2'));
}

/// Branching past the head would ask the server for events that do not exist.
#[test]
fn fork_past_the_head_is_refused() {
    let err = plan_fork("fork", "99", 7).unwrap_err();
    assert!(err.contains("past the current head"), "got: {err}");
    assert!(plan_fork("fork", "nope", 7).is_err());
}

/// The two commands must be distinguishable server-side: `fork_reason` is what the tree view
/// renders, so a rewind that reported "fork" would be indistinguishable from a checkpoint.
#[test]
fn fork_and_undo_use_different_reasons() {
    assert_eq!(plan_fork("fork", "", 3).unwrap().reason, "fork");
    assert_eq!(plan_fork("undo", "", 3).unwrap().reason, "rewind");
}

#[test]
fn arguments_tolerate_surrounding_whitespace() {
    assert_eq!(plan_fork("undo", "  2  ", 9).unwrap().origin_seq, Some(7));
    assert_eq!(plan_fork("fork", " 3 ", 9).unwrap().origin_seq, Some(3));
}
