//! What an operation has to say for itself.
//!
//! Operations used to write their outcome into the TUI's status
//! field, which made every one of them a piece of the TUI. A notice
//! is that outcome as a value: the operation emits it, and whoever
//! installed the sink decides what it looks like. A terminal draws it
//! on the message line; a test reads it.

/// One thing worth telling the user, in the operation's own terms.
///
/// `Info` and `Error` carry prose, which is all most outcomes are.
/// A variant earns its own shape when something other than a person
/// wants to read it: a test asserting on what a sync did should not
/// have to parse a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Info(String),
    Error(String),
    /// A mailbox sync that got through: messages purged, and flag
    /// changes written back.
    Synced {
        deleted: usize,
        updated: usize,
    },
    /// Mail has arrived, where the prose says. A front end asked for
    /// mutt's $beep_new needs to know that without reading the
    /// sentence, which is what earns this its own shape.
    NewMail(String),
}

impl Notice {
    /// Whether this is bad news: the front end colours it, and rings
    /// the bell if the user asked for one.
    pub fn is_error(&self) -> bool {
        matches!(self, Notice::Error(_))
    }

    /// Whether mail has just arrived: $beep_new rings for it.
    pub fn is_new_mail(&self) -> bool {
        matches!(self, Notice::NewMail(_))
    }

    /// The prose for a front end with a line to spare.
    pub fn text(&self) -> String {
        match self {
            Notice::Info(msg) | Notice::Error(msg) | Notice::NewMail(msg) => msg.clone(),
            Notice::Synced { deleted, updated } => {
                format!("synced: {deleted} deleted, {updated} updated")
            }
        }
    }
}

/// Where notices go. The front end installs one and reads it back
/// however it likes.
pub trait NoticeSink: Send {
    fn notice(&mut self, notice: Notice);
    /// The last notice, or nothing since the last `clear`.
    fn latest(&self) -> Option<&Notice>;
    /// Forget it: a new key means the old message has been read.
    fn clear(&mut self);
}

/// The sink a test wants: every notice, in order, behind a handle, so
/// the test can read what was said while whatever it is driving holds
/// a handle of its own.
#[derive(Debug, Clone, Default)]
pub struct Log(std::sync::Arc<std::sync::Mutex<Vec<Notice>>>);

impl Log {
    /// Everything said since the last `clear`, oldest first.
    pub fn notices(&self) -> Vec<Notice> {
        self.0.lock().unwrap().clone()
    }

    /// The prose of the last thing said, empty when nothing was.
    pub fn last_text(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .last()
            .map(Notice::text)
            .unwrap_or_default()
    }

    /// Whether anything said matches; the usual test question.
    pub fn said(&self, needle: &str) -> bool {
        self.0
            .lock()
            .unwrap()
            .iter()
            .any(|n| n.text().contains(needle))
    }
}

impl NoticeSink for Log {
    fn notice(&mut self, notice: Notice) {
        self.0.lock().unwrap().push(notice);
    }

    fn latest(&self) -> Option<&Notice> {
        // A lock cannot hand out a plain reference; a caller that
        // wants the last notice asks for its text.
        None
    }

    fn clear(&mut self) {
        self.0.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_reads_as_prose() {
        let notice = Notice::Synced {
            deleted: 2,
            updated: 3,
        };
        assert_eq!(notice.text(), "synced: 2 deleted, 3 updated");
        assert!(!notice.is_error());
    }

    #[test]
    fn new_mail_is_prose_a_bell_can_recognize() {
        let notice = Notice::NewMail("new mail in inbox (+2)".into());
        assert_eq!(notice.text(), "new mail in inbox (+2)");
        assert!(notice.is_new_mail() && !notice.is_error());
        assert!(!Notice::Info("x".into()).is_new_mail());
    }

    #[test]
    fn a_log_keeps_the_order_and_a_handle_reads_it() {
        let log = Log::default();
        let mut handle = log.clone();
        handle.notice(Notice::Info("first".into()));
        handle.notice(Notice::Error("second".into()));
        assert_eq!(log.notices().len(), 2);
        assert_eq!(log.last_text(), "second");
        assert!(log.said("fir"));
        handle.clear();
        assert_eq!(log.notices(), vec![]);
    }
}
