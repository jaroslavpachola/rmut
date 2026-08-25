//! Message threading after JWZ's algorithm
//! (<https://www.jwz.org/doc/threading.html>): containers per message-id,
//! reference chains linked parent→child with loop protection, empty
//! containers pruned by promoting their children. Threads and siblings
//! are ordered by date. Subject grouping (JWZ step 5) is not done.

use std::collections::HashMap;

use crate::message::Envelope;

/// One index entry in thread order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadedItem {
    /// Index into the input slice.
    pub index: usize,
    /// Nesting depth (0 = thread root).
    pub depth: usize,
    /// Input index of this thread's first (root) message.
    pub root: usize,
}

struct Container {
    message: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
}

fn get_or_create(
    by_id: &mut HashMap<String, usize>,
    arena: &mut Vec<Container>,
    id: &str,
) -> usize {
    if let Some(&c) = by_id.get(id) {
        return c;
    }
    arena.push(Container {
        message: None,
        parent: None,
        children: Vec::new(),
    });
    let idx = arena.len() - 1;
    by_id.insert(id.to_string(), idx);
    idx
}

fn is_ancestor(arena: &[Container], ancestor: usize, mut node: usize) -> bool {
    loop {
        if node == ancestor {
            return true;
        }
        match arena[node].parent {
            Some(p) => node = p,
            None => return false,
        }
    }
}

/// Link parent→child unless it would create a loop or the child is
/// already placed.
fn link(arena: &mut [Container], parent: usize, child: usize) {
    if parent == child || arena[child].parent.is_some() || is_ancestor(arena, child, parent) {
        return;
    }
    arena[child].parent = Some(parent);
    arena[parent].children.push(child);
}

/// Earliest (or, with `newest`, latest) date in a container's
/// subtree, for ordering threads and siblings.
fn subtree_date(arena: &[Container], envs: &[&Envelope], node: usize, newest: bool) -> i64 {
    let own = arena[node]
        .message
        .map(|m| envs[m].date)
        .unwrap_or(if newest { i64::MIN } else { i64::MAX });
    let fold = if newest { i64::max } else { i64::min };
    arena[node]
        .children
        .iter()
        .map(|&c| subtree_date(arena, envs, c, newest))
        .fold(own, fold)
}

/// Children of `node` that carry messages, looking through empty
/// containers (their children are promoted transparently).
fn real_children(arena: &[Container], node: usize, out: &mut Vec<usize>) {
    for &child in &arena[node].children {
        if arena[child].message.is_some() {
            out.push(child);
        } else {
            real_children(arena, child, out);
        }
    }
}

fn emit(
    arena: &[Container],
    envs: &[&Envelope],
    node: usize,
    depth: usize,
    root: usize,
    newest: bool,
    out: &mut Vec<ThreadedItem>,
) {
    let index = arena[node].message.expect("emit called on empty container");
    out.push(ThreadedItem { index, depth, root });
    let mut kids = Vec::new();
    real_children(arena, node, &mut kids);
    kids.sort_by_key(|&k| subtree_date(arena, envs, k, newest));
    for kid in kids {
        emit(arena, envs, kid, depth + 1, root, newest, out);
    }
}

pub fn thread(envs: &[&Envelope]) -> Vec<ThreadedItem> {
    thread_by(envs, ThreadOrder::default())
}

/// Like `thread`, ordering threads by their newest message when
/// `newest` (mutt's sort_aux = last-date-sent).
/// How the threads themselves are ordered, from mutt's $sort_aux:
/// by the root's date or by the newest message under it, oldest
/// first or newest first.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct ThreadOrder {
    /// mutt's `last-`: order a thread by its newest message rather
    /// than by its root.
    pub newest: bool,
    /// mutt's `reverse-`: the newest thread first.
    pub reverse: bool,
}

impl ThreadOrder {
    /// mutt's $sort_aux, as far as threads care: everything else is
    /// a date order under another name.
    pub fn parse(spec: &str) -> ThreadOrder {
        let spec = spec.trim().to_lowercase();
        let (reverse, rest) = match spec.strip_prefix("reverse-") {
            Some(rest) => (true, rest.to_string()),
            None => (false, spec),
        };
        ThreadOrder {
            newest: rest.starts_with("last-"),
            reverse,
        }
    }
}

pub fn thread_by(envs: &[&Envelope], order: ThreadOrder) -> Vec<ThreadedItem> {
    let newest = order.newest;
    let mut arena: Vec<Container> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();

    for (i, env) in envs.iter().enumerate() {
        let id = env
            .msg_id
            .clone()
            .unwrap_or_else(|| format!("<rmut-missing-{i}>"));
        let mut container = get_or_create(&mut by_id, &mut arena, &id);
        if arena[container].message.is_some() {
            // Duplicate Message-ID: give this message its own container.
            arena.push(Container {
                message: None,
                parent: None,
                children: Vec::new(),
            });
            container = arena.len() - 1;
        }
        arena[container].message = Some(i);

        let mut prev: Option<usize> = None;
        for rid in &env.references {
            let r = get_or_create(&mut by_id, &mut arena, rid);
            if r == container {
                continue;
            }
            if let Some(p) = prev {
                link(&mut arena, p, r);
            }
            prev = Some(r);
        }
        if let Some(p) = prev {
            link(&mut arena, p, container);
        }
    }

    // Top-level containers with messages: roots, with empty roots
    // replaced by their (recursively) real children.
    let mut top = Vec::new();
    for i in 0..arena.len() {
        if arena[i].parent.is_none() {
            if arena[i].message.is_some() {
                top.push(i);
            } else {
                real_children(&arena, i, &mut top);
            }
        }
    }
    let envs_ref = envs;
    top.sort_by_key(|&t| subtree_date(&arena, envs_ref, t, newest));
    if order.reverse {
        // mutt's reverse-: the threads turn round, the messages
        // inside one keep their order.
        top.reverse();
    }

    let mut out = Vec::new();
    for t in top {
        let root = arena[t].message.expect("top containers carry messages");
        emit(&arena, envs_ref, t, 0, root, newest, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::maildir::{Flags, MailFile};

    fn env(id: &str, refs: &[&str], date: i64) -> Envelope {
        Envelope {
            file: MailFile {
                path: format!("/mail/{id}").into(),
                is_new: false,
                flags: Flags::default(),
                size: 0,
            },
            from: "x".into(),
            from_full: "x".into(),
            subject: id.into(),
            date,
            msg_id: (!id.is_empty()).then(|| format!("<{id}>")),
            references: refs.iter().map(|r| format!("<{r}>")).collect(),
            tagged: false,
            to: vec![],
            cc: vec![],
            lines: Some(0),
            list: None,
        }
    }

    fn run(envs: &[Envelope]) -> Vec<(usize, usize)> {
        let refs: Vec<&Envelope> = envs.iter().collect();
        thread(&refs).iter().map(|i| (i.index, i.depth)).collect()
    }

    #[test]
    fn chain_nests_by_references() {
        let envs = [
            env("a", &[], 1),
            env("b", &["a"], 2),
            env("c", &["a", "b"], 3),
        ];
        assert_eq!(run(&envs), vec![(0, 0), (1, 1), (2, 2)]);
        let refs: Vec<&Envelope> = envs.iter().collect();
        assert!(thread(&refs).iter().all(|i| i.root == 0));
    }

    #[test]
    fn unrelated_messages_are_separate_threads_by_date() {
        let envs = [env("b", &[], 5), env("a", &[], 2)];
        assert_eq!(run(&envs), vec![(1, 0), (0, 0)]);
    }

    #[test]
    fn missing_parent_promotes_children() {
        // Both reference a message we never saw; they become top-level.
        let envs = [env("b", &["ghost"], 2), env("c", &["ghost"], 3)];
        assert_eq!(run(&envs), vec![(0, 0), (1, 0)]);
    }

    #[test]
    fn missing_middle_of_chain_is_bridged() {
        // c references a and the missing b; c still lands under a.
        let envs = [env("a", &[], 1), env("c", &["a", "b-missing"], 3)];
        assert_eq!(run(&envs), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn newest_orders_threads_by_their_latest_message() {
        // Thread A: root at 10, reply at 100. Thread B: single at 50.
        let envs = [env("a", &[], 10), env("a2", &["a"], 100), env("b", &[], 50)];
        let refs: Vec<&Envelope> = envs.iter().collect();
        let order = |spec: &str| -> Vec<usize> {
            thread_by(&refs, ThreadOrder::parse(spec))
                .iter()
                .map(|t| t.index)
                .collect()
        };
        assert_eq!(order("date"), vec![0, 1, 2], "thread A first by its oldest");
        assert_eq!(
            order("last-date-sent"),
            vec![2, 0, 1],
            "thread B first, A has the newest last"
        );
        // mutt's reverse-: the threads turn round, the messages
        // inside one keep their order.
        assert_eq!(order("reverse-date"), vec![2, 0, 1]);
        assert_eq!(order("reverse-last-date-received"), vec![0, 1, 2]);
    }

    #[test]
    fn sort_aux_spellings_parse_the_way_mutt_writes_them() {
        assert_eq!(ThreadOrder::parse("date"), ThreadOrder::default());
        assert_eq!(
            ThreadOrder::parse("last-date-received"),
            ThreadOrder {
                newest: true,
                reverse: false
            }
        );
        assert_eq!(
            ThreadOrder::parse("reverse-last-date-sent"),
            ThreadOrder {
                newest: true,
                reverse: true
            }
        );
        assert_eq!(
            ThreadOrder::parse("REVERSE-DATE"),
            ThreadOrder {
                newest: false,
                reverse: true
            }
        );
    }

    #[test]
    fn siblings_sorted_by_date() {
        let envs = [
            env("a", &[], 1),
            env("late", &["a"], 9),
            env("early", &["a"], 2),
        ];
        assert_eq!(run(&envs), vec![(0, 0), (2, 1), (1, 1)]);
    }

    #[test]
    fn duplicates_and_self_references_do_not_panic() {
        let envs = [
            env("a", &[], 1),
            env("a", &[], 2),    // duplicate id
            env("s", &["s"], 3), // references itself
            env("", &[], 4),     // no id at all
        ];
        let out = run(&envs);
        assert_eq!(out.len(), 4);
        let mut seen: Vec<usize> = out.iter().map(|(i, _)| *i).collect();
        seen.sort_unstable();
        assert_eq!(seen, vec![0, 1, 2, 3]);
    }

    #[test]
    fn reference_loops_are_broken() {
        // a references b, b references a: whoever links second must not loop.
        let envs = [env("a", &["b"], 1), env("b", &["a"], 2)];
        let out = run(&envs);
        assert_eq!(out.len(), 2);
    }
}
