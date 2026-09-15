//! Session parentage as a tree (96E-53).
//!
//! Pure data: no client, no ratatui, no `App`. Both consumers of the shape — the
//! enriched sidebar and the `/tree` overlay (96E-54) — call [`build_forest`] and
//! render the same [`Forest`], so there is exactly one place where "who is whose
//! child" is decided.
//!
//! The flat `Vec<SessionEntry>` stays authoritative. This is a view over it,
//! addressing entries by index, which is what lets `mark_active`/`selected` and the
//! existing search filter keep working untouched.

use crate::session_manager::SessionEntry;
use std::collections::{HashMap, HashSet};

/// One node: an index into the original slice, plus its children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Index into the `entries` slice passed to [`build_forest`].
    pub idx: usize,
    /// Depth from its root (roots are 0). Precomputed so rendering does not have to
    /// thread a counter through recursion.
    pub depth: usize,
    pub children: Vec<Node>,
}

/// Roots and their descendants, in input order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forest {
    pub roots: Vec<Node>,
}

impl Forest {
    /// Depth-first, parents before children — the order to draw indented rows in.
    pub fn flatten(&self) -> Vec<Node> {
        let mut out = Vec::new();
        fn walk(n: &Node, out: &mut Vec<Node>) {
            out.push(Node {
                idx: n.idx,
                depth: n.depth,
                children: Vec::new(),
            });
            for c in &n.children {
                walk(c, out);
            }
        }
        for r in &self.roots {
            walk(r, &mut out);
        }
        out
    }

    /// Total node count. Equals `entries.len()` for any input, since every entry is
    /// placed exactly once (see the cycle/orphan handling in [`build_forest`]).
    pub fn len(&self) -> usize {
        self.flatten().len()
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

/// Build the forest from a flat session list.
///
/// A session is a root when it has no `parent_id`, **or** when its parent is not in
/// the list. That second case is not a defect to hide: sessions can be deleted, and
/// the picker's search filter narrows the slice it passes here. Promoting an orphan
/// to a root keeps every entry visible, where indexing strictly by parent would make
/// a child silently disappear whenever its parent was filtered out.
///
/// Cycles cannot occur (`origin_session` always points at an older row) but are
/// still handled: any node not reached from a root is promoted, so the output always
/// contains every input exactly once and this can never lose a session.
pub fn build_forest(entries: &[SessionEntry]) -> Forest {
    let index_of: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| (e.id.as_str(), i))
        .collect();

    // parent index -> child indices, in input order.
    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots: Vec<usize> = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        match e
            .parent_id
            .as_deref()
            .and_then(|p| index_of.get(p).copied())
            // A row naming itself as its own parent would recurse forever.
            .filter(|&p| p != i)
        {
            Some(parent) => children.entry(parent).or_default().push(i),
            None => roots.push(i),
        }
    }

    let mut placed: HashSet<usize> = HashSet::new();
    let mut forest = Forest::default();
    for r in roots {
        forest
            .roots
            .push(grow(r, 0, &children, &mut placed));
    }

    // Anything unreachable from a root (only possible via a cycle) becomes a root, so
    // the invariant "every entry appears once" holds unconditionally.
    for i in 0..entries.len() {
        if !placed.contains(&i) {
            forest.roots.push(grow(i, 0, &children, &mut placed));
        }
    }

    forest
}

fn grow(
    idx: usize,
    depth: usize,
    children: &HashMap<usize, Vec<usize>>,
    placed: &mut HashSet<usize>,
) -> Node {
    placed.insert(idx);
    let kids = children
        .get(&idx)
        .map(|v| {
            v.iter()
                .filter(|c| !placed.contains(c))
                .copied()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Node {
        idx,
        depth,
        children: kids
            .into_iter()
            .map(|c| grow(c, depth + 1, children, placed))
            .collect(),
    }
}

/// Short marker for a fork reason, for the sidebar badge and the overlay's edge label.
///
/// Deliberately not emoji: the sidebar is width-constrained and emoji width is
/// unreliable across terminals, which shifts every column after it.
pub fn reason_badge(reason: Option<&str>) -> &'static str {
    match reason {
        Some("fork") => "⑂",
        Some("rewind") => "↺",
        Some("subagent") => "→",
        Some("tree") => "⊹",
        _ => "",
    }
}

/// The picker's row order: indices into the **full** session list, arranged so a branch
/// follows the session it came from, with its depth for indentation.
///
/// 96E-19 is the reason this is one function rather than two loops: the renderer and the
/// key handler MUST agree on the order, or `selected` highlights one row and Enter opens
/// another. Both call this.
///
/// `filter` is applied first, then the tree is built over what survives — so a child whose
/// parent was filtered out is promoted to a root and stays visible.
pub fn picker_order(entries: &[SessionEntry], filter: &str) -> Vec<(usize, usize)> {
    let kept: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, s)| crate::session_manager::session_matches(s, filter))
        .map(|(i, _)| i)
        .collect();
    let subset: Vec<SessionEntry> = kept.iter().map(|&i| entries[i].clone()).collect();
    build_forest(&subset)
        .flatten()
        .into_iter()
        .filter_map(|n| kept.get(n.idx).map(|&orig| (orig, n.depth)))
        .collect()
}

/// What `/fork` or `/undo` resolves to: the fork reason, the seq to branch at, and the note
/// to show. 96E-55.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPlan {
    /// `"fork"` or `"rewind"` — the `reason` sent to `POST /session/{id}/fork`.
    pub reason: &'static str,
    /// The parent seq to branch at. `None` means "at head".
    pub origin_seq: Option<u64>,
    /// Short confirmation for the transcript.
    pub note: String,
}

/// Work out the fork parameters for `/fork [seq]` or `/undo [n]`, or say why not.
///
/// Pure so the argument rules are testable without a server: an append-only log means
/// "drop the last n messages" is a branch that stops n short of `head`, and the arithmetic
/// deciding *where* is exactly the part worth pinning down.
///
/// `Err` carries the message to show the user — every rejection is explained rather than
/// being a silent no-op.
pub fn plan_fork(cmd: &str, args: &str, head: u64) -> Result<ForkPlan, String> {
    let arg = args.trim();
    if cmd == "undo" {
        let n: u64 = if arg.is_empty() {
            1
        } else {
            match arg.parse() {
                Ok(0) | Err(_) => {
                    return Err(format!(
                        "/undo takes a positive number of messages, got {arg:?}"
                    ))
                }
                Ok(n) => n,
            }
        };
        if head < n {
            return Err(format!(
                "Nothing to undo: the session has {head} message(s)."
            ));
        }
        let plural = if n == 1 { "message" } else { "messages" };
        return Ok(ForkPlan {
            reason: "rewind",
            origin_seq: Some(head - n),
            note: format!("{n} {plural} undone."),
        });
    }

    let seq = if arg.is_empty() {
        head
    } else {
        match arg.parse::<u64>() {
            Ok(s) if s <= head => s,
            Ok(s) => {
                return Err(format!(
                    "/fork: seq {s} is past the current head ({head})."
                ))
            }
            Err(_) => return Err(format!("/fork takes a sequence number, got {arg:?}")),
        }
    };
    Ok(ForkPlan {
        reason: "fork",
        origin_seq: Some(seq),
        note: format!("Forked at seq {seq}."),
    })
}
