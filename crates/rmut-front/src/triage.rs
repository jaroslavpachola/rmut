//! The triage review screen, minus the drawing: the digest on top,
//! the proposed marks under it with a tick each, a cursor over the
//! proposals and a view that scrolls. Both front ends draw
//! [`Review::lines`] and hand it their keys.

use rmut_session::Triage;

use crate::{KeyCode, KeyEvent};

/// The help bar's text for the screen.
pub const HELP: &str = "q:Discard Enter:Apply t:Tick a:All j/k:Move Space/-:Page";

/// What a key asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// Apply the ticked proposals, and leave.
    Apply,
    /// Leave, applying nothing.
    Close,
}

/// One row of the screen.
#[derive(Debug, PartialEq, Eq)]
pub enum Line {
    Text(String),
    Heading(String),
    /// Proposal `index`, its tick and text, `selected` under the cursor.
    Item {
        index: usize,
        selected: bool,
        text: String,
    },
}

pub struct Review {
    pub triage: Triage,
    /// The proposal under the cursor.
    pub sel: usize,
    /// The first row in view.
    pub top: usize,
}

impl Review {
    pub fn new(triage: Triage) -> Review {
        Review {
            triage,
            sel: 0,
            top: 0,
        }
    }

    pub fn accepted(&self) -> usize {
        self.triage.proposals.iter().filter(|p| p.accepted).count()
    }

    /// The status line's text.
    pub fn status(&self) -> String {
        format!(
            "---rmut: triage [Proposed:{} Ticked:{}]",
            self.triage.proposals.len(),
            self.accepted()
        )
    }

    /// Every row, top to bottom.
    pub fn lines(&self) -> Vec<Line> {
        let mut out: Vec<Line> = self
            .triage
            .digest
            .iter()
            .map(|l| Line::Text(l.clone()))
            .collect();
        if !self.triage.ignored.is_empty() {
            out.push(Line::Text(String::new()));
            out.push(Line::Heading(format!(
                "Ignored ({}): not an action rmut takes, or no such message",
                self.triage.ignored.len()
            )));
            out.extend(
                self.triage
                    .ignored
                    .iter()
                    .map(|l| Line::Text(format!("  {l}"))),
            );
        }
        out.push(Line::Text(String::new()));
        if self.triage.proposals.is_empty() {
            out.push(Line::Heading("No marks proposed.".into()));
            return out;
        }
        out.push(Line::Heading(format!(
            "Proposed marks ({}): t ticks, Enter applies the ticked ones",
            self.triage.proposals.len()
        )));
        for (index, p) in self.triage.proposals.iter().enumerate() {
            let mut text = format!(
                "[{}] {:<18} {:<5} {:<15.15} {}",
                if p.accepted { 'x' } else { ' ' },
                p.action.name(),
                p.reference,
                p.from,
                p.subject
            );
            if !p.reason.is_empty() {
                text += &format!(" - {}", p.reason);
            }
            out.push(Line::Item {
                index,
                selected: index == self.sel,
                text,
            });
        }
        out
    }

    /// The row proposal `index` sits on.
    fn row_of(&self, index: usize) -> usize {
        let rows = self.lines().len();
        rows - self.triage.proposals.len() + index
    }

    /// Bring the cursor into a view `page` rows high.
    fn fit(&mut self, page: usize) {
        if self.triage.proposals.is_empty() {
            return;
        }
        let row = self.row_of(self.sel);
        let page = page.max(1);
        if row < self.top {
            self.top = row;
        } else if row >= self.top + page {
            self.top = row + 1 - page;
        }
    }

    /// The most `top` may be with `page` rows in view.
    fn max_top(&self, page: usize) -> usize {
        self.lines().len().saturating_sub(page.max(1))
    }

    /// Tick or untick proposal `index` (a click on its row).
    pub fn toggle(&mut self, index: usize) {
        if let Some(p) = self.triage.proposals.get_mut(index) {
            p.accepted = !p.accepted;
            self.sel = index;
        }
    }

    pub fn key(&mut self, key: &KeyEvent, page: usize) -> Step {
        let n = self.triage.proposals.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Step::Close,
            KeyCode::Enter => return Step::Apply,
            KeyCode::Char('j') | KeyCode::Down if n > 0 => {
                self.sel = (self.sel + 1).min(n - 1);
                self.fit(page);
            }
            KeyCode::Char('k') | KeyCode::Up if n > 0 => {
                self.sel = self.sel.saturating_sub(1);
                self.fit(page);
            }
            // mutt's t: toggle, then move on.
            KeyCode::Char('t') if n > 0 => {
                self.toggle(self.sel);
                self.sel = (self.sel + 1).min(n - 1);
                self.fit(page);
            }
            // All ticked, or none if they all were.
            KeyCode::Char('a') => {
                let on = self.accepted() < n;
                for p in &mut self.triage.proposals {
                    p.accepted = on;
                }
            }
            KeyCode::Char(' ') | KeyCode::PageDown => {
                self.top = (self.top + page.max(1)).min(self.max_top(page));
            }
            KeyCode::Char('-') | KeyCode::PageUp => {
                self.top = self.top.saturating_sub(page.max(1));
            }
            KeyCode::Home => self.top = 0,
            KeyCode::End => self.top = self.max_top(page),
            _ => {}
        }
        Step::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::KeyModifiers;

    fn review() -> Review {
        // The session's parser is the only way to make proposals.
        let triage = rmut_session::Triage {
            digest: (1..=10).map(|i| format!("digest line {i}")).collect(),
            ..Default::default()
        };
        Review::new(triage)
    }

    fn press(r: &mut Review, c: char, page: usize) -> Step {
        r.key(&KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), page)
    }

    #[test]
    fn the_digest_scrolls_and_q_discards() {
        let mut r = review();
        assert_eq!(r.lines().len(), 12, "digest, blank, heading");
        assert_eq!(press(&mut r, ' ', 5), Step::Stay);
        assert_eq!(r.top, 5);
        press(&mut r, ' ', 5);
        assert_eq!(r.top, 7, "no further than the last page");
        press(&mut r, '-', 5);
        assert_eq!(r.top, 2);
        assert_eq!(press(&mut r, 'q', 5), Step::Close);
        assert_eq!(
            r.key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 5),
            Step::Apply
        );
    }

    #[test]
    fn t_ticks_and_moves_and_a_flips_them_all() {
        use rmut_session::{Proposal, TriageAction};
        let mut r = review();
        r.triage.proposals = vec![
            Proposal::detached(TriageAction::Flag, "m1", "one"),
            Proposal::detached(TriageAction::Read, "m2", "two"),
            Proposal::detached(TriageAction::Label("news".into()), "m3", "three"),
        ];
        assert_eq!(r.accepted(), 3, "proposals start ticked");
        // The cursor's row comes into a view of 5 once it moves.
        press(&mut r, 't', 5);
        assert!(!r.triage.proposals[0].accepted);
        assert_eq!(r.sel, 1);
        assert_eq!(r.top, 9, "row 13 (proposal 2) at the bottom of the view");
        let Line::Item { text, selected, .. } = &r.lines()[13] else {
            panic!("an item");
        };
        assert!(*selected && text.starts_with("[x] read"), "{text}");
        press(&mut r, 'a', 5);
        assert_eq!(r.accepted(), 3, "not all ticked: a ticks them all");
        press(&mut r, 'a', 5);
        assert_eq!(r.accepted(), 0);
        assert_eq!(r.status(), "---rmut: triage [Proposed:3 Ticked:0]");
    }
}
