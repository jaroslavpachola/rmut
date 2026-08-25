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
}

impl Notice {
    /// Whether this is bad news: the front end colours it, and rings
    /// the bell if the user asked for one.
    pub fn is_error(&self) -> bool {
        matches!(self, Notice::Error(_))
    }

    /// The prose for a front end with a line to spare.
    pub fn text(&self) -> String {
        match self {
            Notice::Info(msg) | Notice::Error(msg) => msg.clone(),
            Notice::Synced { deleted, updated } => {
                format!("synced: {deleted} deleted, {updated} updated")
            }
        }
    }
}

/// Where notices go. The front end installs one and reads it back
/// however it likes.
pub trait NoticeSink {
    fn notice(&mut self, notice: Notice);
    /// The last notice, or nothing since the last `clear`.
    fn latest(&self) -> Option<&Notice>;
    /// Forget it: a new key means the old message has been read.
    fn clear(&mut self);
}

/// The sink a test wants: every notice, in order.
#[derive(Debug, Default)]
pub struct Log(pub Vec<Notice>);

impl NoticeSink for Log {
    fn notice(&mut self, notice: Notice) {
        self.0.push(notice);
    }

    fn latest(&self) -> Option<&Notice> {
        self.0.last()
    }

    fn clear(&mut self) {
        self.0.clear();
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
    fn a_log_keeps_the_order_and_the_latest() {
        let mut log = Log::default();
        log.notice(Notice::Info("first".into()));
        log.notice(Notice::Error("second".into()));
        assert_eq!(log.0.len(), 2);
        assert!(log.latest().is_some_and(Notice::is_error));
        log.clear();
        assert_eq!(log.latest(), None);
    }
}
