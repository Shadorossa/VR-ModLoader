//! Nested view of a flat T2B entry list (docs/formats/cfgbin.md §2).
//!
//! Port of `t2b_tree()` in `tools/py/cfgbin.py`. Nodes reference entries by index so
//! the tree can be sent to the UI next to the flat `entries` array.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use super::{Entry, SORT_INDEX_NAME, T2b, Value};
use crate::error::Result;

/// Maximum block nesting before BEGIN entries are treated as plain entries
/// (guards the recursion against hostile files; real files nest < 10 deep).
const MAX_DEPTH: usize = 256;

/// Tree-building strategy (cfgbin.md §2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TreeMode {
    /// END search bounded by the parent block; BEGIN without END is a counted list
    /// `X_LIST_BEG(count[, hasSortIndex])` whose rows absorb `ROW_*` attributes
    /// (`ROW_REF_Y`, nested `ROW_Z_LIST_BEG`) and whose `__SORT_INDEX` entries are collected.
    /// A block *with* END gets the same row structure when its `count` rows (plus sort
    /// index) fill it exactly — an extension of the Python reference, which only does this
    /// for END-less lists (60 of the 68 VR lists with `__SORT_INDEX` have an END).
    #[default]
    Counted,
    /// Exact CfgBinEditor behaviour: END searched to end of file; a BEGIN without END
    /// swallows the rest of the file.
    Editor,
}

/// A node of the nested view.
///
/// JSON: `{"entry": 3, "children": [...], "end": 9, "sortIndex": [10, 11]}` — all numbers
/// are indices into `T2b::entries`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// Index of the entry this node represents (BEGIN entry for blocks, the row for rows).
    pub entry: usize,
    /// Children: block contents, list rows, or row attributes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Node>,
    /// Index of the closing `X_END` entry, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<usize>,
    /// Indices of the `__SORT_INDEX` entries of a `X_LIST_BEG(n, 1)` list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_index: Option<Vec<usize>>,
}

impl Node {
    fn leaf(entry: usize) -> Self {
        Node {
            entry,
            children: Vec::new(),
            end: None,
            sort_index: None,
        }
    }

    /// Largest entry index covered by this node (itself, children, END, sort index).
    pub fn last_index(&self) -> usize {
        let mut m = self.entry;
        if let Some(c) = self.children.last() {
            m = m.max(c.last_index());
        }
        if let Some(e) = self.end {
            m = m.max(e);
        }
        if let Some(s) = self.sort_index.as_ref().and_then(|s| s.last()) {
            m = m.max(*s);
        }
        m
    }
}

/// Port of CfgBinEditor `GetBeginIndex`: position of the BEGIN suffix, or `None`.
fn begin_index(name: &str) -> Option<usize> {
    if name == "PTREE" {
        return Some(0);
    }
    for suf in ["_BEGIN", "_BEG", "_BGN"] {
        if let Some(i) = name.rfind(suf) {
            return Some(i);
        }
    }
    name.ends_with('_').then(|| name.len() - 1)
}

/// (base, end name) of a BEGIN entry.
fn end_name(name: &str, bi: usize) -> (&str, String) {
    let (base, part) = if bi == 0 {
        (name, "")
    } else {
        (&name[..bi], &name[bi..])
    };
    let end = if base == "PTREE" || part == "_" {
        format!("_{base}")
    } else {
        format!("{base}_END")
    };
    (base, end)
}

struct Counted {
    rows: Vec<Node>,
    sort_index: Option<Vec<usize>>,
    /// Index of the first entry after the list.
    next: usize,
    /// All `count` rows were found.
    complete: bool,
}

struct Builder<'a> {
    entries: &'a [Entry],
    names: Vec<Cow<'a, str>>,
    /// Number of `build` calls so far. Trying the counted layout inside END blocks can
    /// re-parse a block; past `budget` that attempt is skipped so hostile files cannot
    /// cause exponential work (real files stay far below it).
    work: std::cell::Cell<usize>,
    budget: usize,
}

impl<'a> Builder<'a> {
    fn name(&self, i: usize) -> &str {
        &self.names[i]
    }

    fn find_end(&self, i: usize, limit: usize) -> Option<usize> {
        let name = self.name(i);
        let bi = begin_index(name)?;
        let (base, end) = end_name(name, bi);
        (i + 1..limit).find(|&j| {
            let n = self.name(j);
            n == end || (n == base && self.entries[j].values.is_empty())
        })
    }

    // ---- editor mode -------------------------------------------------------

    fn populate(
        &self,
        mut idx: usize,
        stop: Option<usize>,
        out: &mut Vec<Node>,
        depth: usize,
    ) -> usize {
        let n_all = self.entries.len();
        while idx < n_all {
            if stop == Some(idx) {
                break;
            }
            let mut node = Node::leaf(idx);
            if depth < MAX_DEPTH && begin_index(self.name(idx)).is_some() {
                let end_i = self.find_end(idx, n_all);
                idx = self.populate(idx + 1, end_i, &mut node.children, depth + 1);
                if idx < n_all {
                    node.end = Some(idx);
                }
            }
            out.push(node);
            idx += 1;
        }
        idx
    }

    // ---- counted mode ------------------------------------------------------

    fn build(&self, i: usize, limit: usize, depth: usize) -> (Node, usize) {
        self.work.set(self.work.get() + 1);
        let mut node = Node::leaf(i);
        if depth >= MAX_DEPTH || begin_index(self.name(i)).is_none() {
            return (node, i + 1);
        }
        if let Some(j) = self.find_end(i, limit) {
            // A block with END. If it is a counted list whose rows (+ sort index) fill the
            // block exactly, use the row structure; otherwise plain children (as the Python
            // reference does for every END block).
            let attempt = if self.work.get() < self.budget {
                self.counted(i, j, depth)
            } else {
                None
            };
            match attempt {
                Some(c) if c.next == j && c.complete => {
                    node.children = c.rows;
                    node.sort_index = c.sort_index;
                }
                _ => node.children = self.parse(i + 1, j, depth + 1),
            }
            node.end = Some(j);
            return (node, j + 1);
        }
        // counted list without END: X_LIST_BEG(count[, has_sort_index])
        match self.counted(i, limit, depth) {
            Some(c) => {
                node.children = c.rows;
                node.sort_index = c.sort_index;
                (node, c.next)
            }
            None => (node, i + 1),
        }
    }

    /// Parse the rows of `X_LIST_BEG(count[, hasSortIndex])` at `i`, bounded by `limit`.
    /// `None` if the entry has no positive int count or nothing follows it.
    fn counted(&self, i: usize, limit: usize, depth: usize) -> Option<Counted> {
        let e = &self.entries[i];
        let cnt = match e.values.first() {
            Some(Value::Int(c)) if *c > 0 => *c as usize,
            _ => return None,
        };
        let has_sort = matches!(e.values.get(1), Some(Value::Int(1)));
        let mut k = i + 1;
        if k >= limit {
            return None;
        }
        let item = self.name(k).to_owned();
        let prefix = format!("{item}_");
        let item_is_block = begin_index(&item).is_some();
        let mut rows = Vec::new();
        while rows.len() < cnt {
            if k >= limit || self.name(k) != item {
                break;
            }
            let child = if item_is_block {
                let (child, next) = self.build(k, limit, depth + 1);
                k = next;
                child
            } else {
                let mut child = Node::leaf(k);
                k += 1;
                // row attributes: ITEM_REF_xxx(start, count), nested ITEM_xxx_LIST_BEG blocks
                while k < limit && self.name(k) != item && self.name(k).starts_with(&prefix) {
                    let (sub, next) = self.build(k, limit, depth + 1);
                    child.children.push(sub);
                    k = next;
                }
                child
            };
            rows.push(child);
        }
        let complete = rows.len() == cnt;
        let mut sort_index = None;
        if has_sort {
            let mut idx = Vec::new();
            while k < limit && self.name(k) == SORT_INDEX_NAME && idx.len() < cnt {
                idx.push(k);
                k += 1;
            }
            sort_index = Some(idx);
        }
        Some(Counted {
            rows,
            sort_index,
            next: k,
            complete,
        })
    }

    fn parse(&self, mut i: usize, limit: usize, depth: usize) -> Vec<Node> {
        let mut out = Vec::new();
        while i < limit {
            let (node, next) = self.build(i, limit, depth);
            out.push(node);
            i = next;
        }
        out
    }
}

/// Group a flat entry list into nodes (cfgbin.md §2.3). Every entry is covered exactly once.
pub fn build_tree(entries: &[Entry], mode: TreeMode) -> Vec<Node> {
    let b = Builder {
        entries,
        names: entries.iter().map(Entry::display_name).collect(),
        work: std::cell::Cell::new(0),
        budget: entries.len().saturating_mul(8).saturating_add(4096),
    };
    match mode {
        TreeMode::Editor => {
            let mut roots = Vec::new();
            b.populate(0, None, &mut roots, 0);
            roots
        }
        TreeMode::Counted => b.parse(0, entries.len(), 0),
    }
}

/// Total order used for sort keys: ints as unsigned 32-bit, then floats, then strings.
#[derive(PartialEq, PartialOrd)]
enum RowKey<'a> {
    Missing,
    Int(u32),
    Float(OrdF32),
    Str(Option<&'a str>),
}

#[derive(PartialEq)]
struct OrdF32(f32);
impl PartialOrd for OrdF32 {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        // normalise -0.0 to 0.0 so equal numbers stay stable-equal; total_cmp keeps it a total order
        let n = |v: f32| if v == 0.0 { 0.0 } else { v };
        Some(n(self.0).total_cmp(&n(o.0)))
    }
}

fn row_key(e: &Entry, field: usize) -> RowKey<'_> {
    match e.values.get(field) {
        None => RowKey::Missing,
        Some(Value::Int(v)) => RowKey::Int(*v as u32),
        Some(Value::Float(f)) => RowKey::Float(OrdF32(*f)),
        Some(Value::String(s)) => RowKey::Str(s.as_deref()),
    }
}

fn cmp_keys(a: &RowKey<'_>, b: &RowKey<'_>) -> std::cmp::Ordering {
    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
}

/// Stable argsort of the first `n` rows by unsigned `field`.
fn sort_permutation(entries: &[Entry], rows: &[Node], field: usize, n: usize) -> Vec<usize> {
    let keys: Vec<RowKey<'_>> = rows[..n]
        .iter()
        .map(|r| row_key(&entries[r.entry], field))
        .collect();
    let mut perm: Vec<usize> = (0..n).collect();
    perm.sort_by(|&a, &b| cmp_keys(&keys[a], &keys[b]));
    perm
}

/// A counted list that carries a `__SORT_INDEX` (`X_LIST_BEG(count, 1)`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SortedList {
    /// Index of the `X_LIST_BEG` entry.
    pub begin: usize,
    /// Name of the `X_LIST_BEG` entry (lists are matched by name when rebuilding).
    pub name: String,
    /// Number of rows found.
    pub rows: usize,
    /// Value index the index is ordered by (usually 0; 2 in `trophy_config`'s `TROPHY_INFO`).
    pub key_field: usize,
}

/// Detect which value the stored index sorts by, using the first `m` rows covered by it:
/// the first field whose stable argsort reproduces it, else the first field that is
/// non-decreasing in index order, else field 0 (same rule as RDBN, rdbn.md §7).
fn detect_key_field(entries: &[Entry], list: &Node) -> usize {
    let Some(idx) = &list.sort_index else {
        return 0;
    };
    let perm: Vec<usize> = idx
        .iter()
        .map(|&i| {
            entries[i]
                .values
                .first()
                .and_then(Value::as_int)
                .map_or(usize::MAX, |v| v as u32 as usize)
        })
        .collect();
    let m = perm.len();
    let mut seen = vec![false; m];
    let valid = m <= list.children.len()
        && perm
            .iter()
            .all(|&p| p < m && !std::mem::replace(&mut seen[p], true));
    if !valid || m == 0 {
        return 0;
    }
    let rows = &list.children[..m];
    let width = rows
        .iter()
        .map(|r| entries[r.entry].values.len())
        .max()
        .unwrap_or(0);
    if let Some(f) = (0..width).find(|&f| sort_permutation(entries, rows, f, m) == perm) {
        return f;
    }
    (0..width)
        .find(|&f| {
            perm.windows(2).all(|w| {
                cmp_keys(
                    &row_key(&entries[rows[w[0]].entry], f),
                    &row_key(&entries[rows[w[1]].entry], f),
                )
                .is_le()
            })
        })
        .unwrap_or(0)
}

fn collect_sorted<'n>(nodes: &'n [Node], out: &mut Vec<&'n Node>) {
    for n in nodes {
        collect_sorted(&n.children, out);
        if n.sort_index.is_some() {
            out.push(n);
        }
    }
}

pub(super) fn sorted_lists(doc: &T2b) -> Vec<SortedList> {
    let tree = build_tree(&doc.entries, TreeMode::Counted);
    let mut nodes = Vec::new();
    collect_sorted(&tree, &mut nodes);
    nodes
        .into_iter()
        .map(|n| SortedList {
            begin: n.entry,
            name: doc.entries[n.entry].display_name().into_owned(),
            rows: n.children.len(),
            key_field: detect_key_field(&doc.entries, n),
        })
        .collect()
}

pub(super) fn rebuild_sort_indexes(doc: &mut T2b, keys: Option<&[SortedList]>) -> Result<usize> {
    let tree = build_tree(&doc.entries, TreeMode::Counted);
    let mut nodes = Vec::new();
    collect_sorted(&tree, &mut nodes);
    // (insert position, number of existing sort entries, new permutation)
    let mut jobs: Vec<(usize, usize, Vec<usize>)> = Vec::with_capacity(nodes.len());
    let mut name_seen = std::collections::HashMap::<String, usize>::new();
    for n in nodes {
        let existing = n.sort_index.as_deref().unwrap_or_default();
        let name = doc.entries[n.entry].display_name().into_owned();
        // k-th list with this name (names can repeat, e.g. nested per-row lists)
        let ord = {
            let c = name_seen.entry(name.clone()).or_insert(0);
            *c += 1;
            *c - 1
        };
        let key_field = match keys {
            Some(ks) => ks
                .iter()
                .filter(|k| k.name == name)
                .nth(ord)
                .map_or(0, |k| k.key_field),
            None => detect_key_field(&doc.entries, n),
        };
        let pos = match existing.first() {
            Some(&p) => p,
            None => n.children.last().map_or(n.entry, Node::last_index) + 1,
        };
        let perm = sort_permutation(&doc.entries, &n.children, key_field, n.children.len());
        jobs.push((pos, existing.len(), perm));
    }
    jobs.sort_by_key(|j| std::cmp::Reverse(j.0));
    let hash = doc.hash_name(SORT_INDEX_NAME)?;
    let n_jobs = jobs.len();
    for (pos, old_len, perm) in jobs {
        let new = perm.into_iter().map(|r| Entry {
            name: Some(SORT_INDEX_NAME.to_owned()),
            hash,
            values: vec![Value::Int(r as i32)],
        });
        doc.entries.splice(pos..pos + old_len, new);
    }
    Ok(n_jobs)
}
