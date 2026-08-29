//! Message threading after JWZ's algorithm
//! (<https://www.jwz.org/doc/threading.html>): containers per message-id,
//! reference chains linked parent→child with loop protection, empty
//! containers pruned by promoting their children. Threads and siblings
//! are ordered by date. JWZ step 5, grouping what is left by subject,
//! is mutt's `pseudo_threads` here: see [`SubjectFallback`].

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
    /// The subject fallback put this message here, not its own
    /// References: mutt's fake_thread, which its tree stars.
    pub pseudo: bool,
}

struct Container {
    message: Option<usize>,
    parent: Option<usize>,
    children: Vec<usize>,
    /// Attached by subject rather than by a reference chain.
    pseudo: bool,
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
        pseudo: false,
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

/// Link parent→child unless it would create a loop, the child is
/// already placed, or the child is a message that carries no
/// references of its own. That last one is what makes break-thread
/// stick: a reply's References chain still names the broken message's
/// old ancestors, and without it the chain would quietly put the
/// message back under them.
fn link(arena: &mut [Container], rooted: &[bool], parent: usize, child: usize) {
    if parent == child
        || arena[child].parent.is_some()
        || rooted[child]
        || is_ancestor(arena, child, parent)
    {
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
    out.push(ThreadedItem {
        index,
        depth,
        root,
        pseudo: arena[node].pseudo,
    });
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

/// mutt's subject fallback, which runs unless `$strict_threads`: a
/// thread root whose subject repeats one already in the mailbox
/// hangs under the message carrying it. It is what keeps mail that
/// arrives with no References at all (a notification robot, a list
/// that strips the headers) reading as one thread.
#[derive(Clone, Copy)]
pub struct SubjectFallback<'a> {
    /// Compiled $reply_regexp: what marks a subject as a reply, and
    /// what is taken off it to compare with (mutt's real_subj).
    pub reply_re: &'a regex_lite::Regex,
    /// mutt's $sort_re: only a root whose subject carries the reply
    /// prefix is attached. Unset, any equal subject is, which groups
    /// unrelated mail sharing a subject like "hi".
    pub sort_re: bool,
}

/// mutt's real_subj: the subject with one $reply_regexp match taken
/// off the front, and whether it was there at all. Both ends are
/// trimmed, where mutt compares the raw remainder, so a stray
/// trailing space does not split a thread.
fn real_subject<'a>(re: &regex_lite::Regex, subject: &'a str) -> (&'a str, bool) {
    match re.find(subject) {
        Some(m) if m.start() == 0 => (subject[m.end()..].trim(), true),
        _ => (subject.trim(), false),
    }
}

/// The nearest ancestor carrying a message, looking through the
/// empty containers a missing parent leaves behind.
fn message_ancestor(arena: &[Container], node: usize) -> Option<usize> {
    let mut at = arena[node].parent;
    while let Some(p) = at {
        if arena[p].message.is_some() {
            return Some(p);
        }
        at = arena[p].parent;
    }
    None
}

/// mutt's pseudo_threads: every thread root whose subject repeats a
/// subject already in the mailbox is hung under the message that
/// carries it, roots taken oldest first. The parent may sit anywhere
/// in a thread, not only at its root, but it must be a message whose
/// own subject differs from its parent's (mutt's subject_changed) and
/// not one that was itself attached this way, so a stray answers the
/// message that named the subject rather than the last one to repeat
/// it: the shape mutt draws is one root with a flat fan under it.
fn group_by_subject(
    arena: &mut [Container],
    envs: &[&Envelope],
    top: &mut Vec<usize>,
    sub: &SubjectFallback,
) {
    // Who may be a parent, by the subject they named. A reply
    // repeating its parent's subject is not in here, so a long thread
    // does not offer every message in it as a place to hang strays.
    let mut candidates: HashMap<&str, Vec<usize>> = HashMap::new();
    for node in 0..arena.len() {
        let Some(m) = arena[node].message else {
            continue;
        };
        let subj = real_subject(sub.reply_re, &envs[m].subject).0;
        let changed = match message_ancestor(arena, node) {
            Some(p) => {
                let pm = arena[p]
                    .message
                    .expect("message_ancestor carries a message");
                real_subject(sub.reply_re, &envs[pm].subject).0 != subj
            }
            None => true,
        };
        if changed {
            candidates.entry(subj).or_default().push(node);
        }
    }

    // Oldest first, so the message that opened the subject is the one
    // still standing as a root when the later ones look for a parent.
    let mut roots: Vec<usize> = top.clone();
    roots.sort_by_key(|&r| {
        let m = arena[r].message.expect("a thread root carries a message");
        (envs[m].date, m)
    });

    for cur in roots {
        let m = arena[cur].message.expect("a thread root carries a message");
        // What break-thread marked stays where the user put it. mutt
        // has nowhere to record that and hangs a broken message
        // straight back under its old subject; rmut's `#` sticks.
        if envs[m].broken {
            continue;
        }
        let (subj, is_reply) = real_subject(sub.reply_re, &envs[m].subject);
        if sub.sort_re && !is_reply {
            continue;
        }
        let here = (envs[m].date, m);
        let mut best: Option<((i64, usize), usize)> = None;
        for &t in candidates.get(subj).map(Vec::as_slice).unwrap_or_default() {
            if t == cur || arena[t].pseudo {
                continue;
            }
            let tm = arena[t].message.expect("a candidate carries a message");
            let there = (envs[tm].date, tm);
            // Only a message already sent, and never one from inside
            // this root's own thread: that would be a loop.
            if there >= here || is_ancestor(arena, cur, t) {
                continue;
            }
            if best.is_none_or(|(seen, _)| seen < there) {
                best = Some((there, t));
            }
        }
        if let Some((_, parent)) = best {
            arena[cur].parent = Some(parent);
            arena[parent].children.push(cur);
            arena[cur].pseudo = true;
        }
    }
    top.retain(|&t| arena[t].parent.is_none());
}

pub fn thread_by(envs: &[&Envelope], order: ThreadOrder) -> Vec<ThreadedItem> {
    thread_with(envs, order, None)
}

/// Threading proper: reference chains, and then, with a
/// [`SubjectFallback`], what mutt does for the messages whose
/// senders left no chain behind.
pub fn thread_with(
    envs: &[&Envelope],
    order: ThreadOrder,
    subject: Option<&SubjectFallback>,
) -> Vec<ThreadedItem> {
    let newest = order.newest;
    let mut arena: Vec<Container> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();

    // Every message gets its container first, so a message that
    // explicitly carries no references is known as a root before any
    // other message's chain can claim it.
    let mut container_of = Vec::with_capacity(envs.len());
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
                pseudo: false,
            });
            container = arena.len() - 1;
        }
        arena[container].message = Some(i);
        container_of.push(container);
    }
    let mut rooted: Vec<bool> = arena
        .iter()
        .map(|c| c.message.is_some_and(|m| envs[m].references.is_empty()))
        .collect();

    for (i, env) in envs.iter().enumerate() {
        let container = container_of[i];
        let mut prev: Option<usize> = None;
        for rid in &env.references {
            let r = get_or_create(&mut by_id, &mut arena, rid);
            rooted.resize(arena.len(), false);
            if r == container {
                continue;
            }
            if let Some(p) = prev {
                link(&mut arena, &rooted, p, r);
            }
            prev = Some(r);
        }
        if let Some(p) = prev {
            link(&mut arena, &rooted, p, container);
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
    if let Some(sub) = subject {
        group_by_subject(&mut arena, envs, &mut top, sub);
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
            label: None,
            broken: false,
        }
    }

    /// Like `env`, with a subject of its own: what the subject
    /// fallback works from.
    fn subj(id: &str, subject: &str, refs: &[&str], date: i64) -> Envelope {
        Envelope {
            subject: subject.into(),
            ..env(id, refs, date)
        }
    }

    fn run_subject(envs: &[Envelope], sort_re: bool) -> Vec<(usize, usize, bool)> {
        let re = crate::compose::default_reply_regexp();
        let fallback = SubjectFallback {
            reply_re: &re,
            sort_re,
        };
        let refs: Vec<&Envelope> = envs.iter().collect();
        thread_with(&refs, ThreadOrder::default(), Some(&fallback))
            .iter()
            .map(|i| (i.index, i.depth, i.pseudo))
            .collect()
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

    #[test]
    fn a_message_without_references_is_never_reparented_by_its_replies() {
        // b answered a, and c answered b, so c's chain names a and b.
        // break-thread then cleared b's own headers: b must stand as
        // a root with c under it, and c's chain must not put it back
        // under a (mutt's mutt_break_thread edits the one message).
        let envs = [env("a", &[], 1), env("b", &[], 2), env("c", &["a", "b"], 3)];
        assert_eq!(run(&envs), vec![(0, 0), (1, 0), (2, 1)]);
        // A message with references of its own still hangs where its
        // chain says, including through a missing ancestor.
        let envs = [
            env("a", &[], 1),
            env("b", &["a"], 2),
            env("c", &["a", "b"], 3),
        ];
        assert_eq!(run(&envs), vec![(0, 0), (1, 1), (2, 2)]);
        let envs = [env("p", &[], 1), env("c", &["gone", "p"], 2)];
        assert_eq!(run(&envs), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn subject_groups_mail_that_carries_no_references() {
        // The gitlab-notification shape: every message a fresh
        // Message-ID, no References anywhere, one subject. mutt hangs
        // the later ones off the first as a flat fan; without the
        // fallback they are seven threads.
        let envs = [
            subj("a", "Re: proj | a change (!1661)", &[], 10),
            subj("b", "Re: proj | a change (!1661)", &[], 20),
            subj("c", "Re: proj | a change (!1661)", &[], 30),
        ];
        assert_eq!(
            run_subject(&envs, true),
            vec![(0, 0, false), (1, 1, true), (2, 1, true)]
        );
        // Without it, three roots, which is what rmut did before.
        assert_eq!(run(&envs), vec![(0, 0), (1, 0), (2, 0)]);
    }

    #[test]
    fn sort_re_decides_whether_a_plain_subject_joins() {
        // "hi", then a reply to it, then another unrelated "hi".
        let envs = [
            subj("a", "hi", &[], 10),
            subj("b", "Re: hi", &[], 20),
            subj("c", "hi", &[], 30),
            subj("d", "Re: other", &[], 40),
        ];
        // $sort_re set (mutt's default): only the "Re:" one joins.
        assert_eq!(
            run_subject(&envs, true),
            vec![(0, 0, false), (1, 1, true), (2, 0, false), (3, 0, false)]
        );
        // Unset: any equal subject joins, which is what makes a
        // mailbox full of "hi" one thread.
        assert_eq!(
            run_subject(&envs, false),
            vec![(0, 0, false), (1, 1, true), (2, 1, true), (3, 0, false)]
        );
    }

    #[test]
    fn a_renamed_reply_is_the_parent_for_its_own_subject() {
        // b keeps a's subject, m renames the thread. A stray "Re:
        // newtopic" belongs under m, not under the root: mutt's
        // subject_changed, which keeps every message in a long thread
        // from offering itself as a parent.
        let envs = [
            subj("a", "hi", &[], 10),
            subj("b", "Re: hi", &["a"], 20),
            subj("m", "Re: newtopic", &["a", "b"], 30),
            subj("n", "Re: newtopic", &[], 40),
        ];
        assert_eq!(
            run_subject(&envs, true),
            vec![(0, 0, false), (1, 1, false), (2, 2, false), (3, 3, true)]
        );
    }

    #[test]
    fn a_subject_child_brings_its_own_replies_with_it() {
        // c answered b by References; b, which carries none of its
        // own, joins a by subject and c goes along, a level deeper.
        let envs = [
            subj("a", "hi", &[], 10),
            subj("b", "Re: hi", &[], 20),
            subj("c", "Re: hi", &["b"], 30),
        ];
        assert_eq!(
            run_subject(&envs, true),
            vec![(0, 0, false), (1, 1, true), (2, 2, false)]
        );
    }

    #[test]
    fn the_subject_pass_never_loops_or_reparents_a_real_child() {
        // A message already placed by its chain stays there, and the
        // oldest root of a subject is nobody's child.
        let envs = [
            subj("a", "Re: same", &[], 30),
            subj("b", "Re: same", &["a"], 10),
        ];
        let out = run_subject(&envs, true);
        assert_eq!(out.len(), 2);
        // b hangs off a by reference; a, though later, cannot then
        // hang off b.
        assert_eq!(out, vec![(0, 0, false), (1, 1, false)]);
    }
}
