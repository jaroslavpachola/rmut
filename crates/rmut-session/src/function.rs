//! What the index can be told to do, and the doing of it.
//!
//! mutt names every operation (`<delete-message>`, `<group-reply>`)
//! and binds keys to the names, which is what makes a keymap a
//! configuration rather than a program. rmut had the names, but they
//! lived in the terminal front end next to the dispatch, so the only
//! way to reach `delete` was to press a key on a pty: 300 lines of
//! mailbox logic that no test could name.
//!
//! The names and the dispatch live here now. A front end resolves
//! whatever it has (a keystroke, a menu item, `:exec`) to a
//! [`Function`], hands it to [`Session::run_function`], and reads the
//! [`Outcome`]. Most functions finish inside the session; some stop
//! to [`Ask`]; the rest come back as a [`FrontOp`], which is the
//! honest list of what a session cannot do for itself because it owns
//! neither a screen nor an editor.

use crate::ask::{Ask, PatternOp};
use crate::{ComposeKind, Session, ThreadOp};

/// One thing the index can be told to do, under the name mutt gives
/// it. A front end binds keys or menu items to these; `bind` and
/// `:exec` name them straight out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Function {
    Quit,
    Abort,
    Down,
    Up,
    PageDown,
    PageUp,
    First,
    Last,
    View,
    Delete,
    Undelete,
    Flag,
    ToggleNew,
    Sync,
    Compose,
    Reply,
    GroupReply,
    ListReply,
    Forward,
    Sort,
    Limit,
    Search,
    SearchReverse,
    SearchNext,
    NextNew,
    PrevNew,
    ChangeMailbox,
    ChangeMailboxReadOnly,
    Folders,
    Attachments,
    FoldThread,
    FoldAll,
    Print,
    Tag,
    TagPrefix,
    DeleteThread,
    UndeleteThread,
    TagThread,
    DeleteSubthread,
    UndeleteSubthread,
    NextThread,
    PrevThread,
    BreakThread,
    LinkThreads,
    ReadThread,
    ReadSubthread,
    TagSubthread,
    ParentMessage,
    RootMessage,
    EditLabel,
    ShowVersion,
    ShowLimit,
    ToggleWrite,
    DisplayAddress,
    PageTop,
    PageMiddle,
    PageBottom,
    Undo,
    DeletePattern,
    UndeletePattern,
    TagPattern,
    UntagPattern,
    FetchMail,
    Save,
    Copy,
    DecodeSave,
    DecodeCopy,
    Pipe,
    Bounce,
    Resend,
    Edit,
    SidebarToggle,
    SidebarNext,
    SidebarPrev,
    SidebarOpen,
    CreateAlias,
    Query,
    Notmuch,
    EnterCommand,
    Shell,
    Redraw,
    Suspend,
    Help,
}

impl Function {
    pub fn name(self) -> &'static str {
        use Function::*;
        match self {
            Quit => "quit",
            Abort => "abort",
            Down => "down",
            Up => "up",
            PageDown => "page-down",
            PageUp => "page-up",
            First => "first",
            Last => "last",
            View => "view",
            Delete => "delete",
            Undelete => "undelete",
            Flag => "flag",
            ToggleNew => "toggle-new",
            Sync => "sync",
            Compose => "compose",
            Reply => "reply",
            GroupReply => "group-reply",
            ListReply => "list-reply",
            Forward => "forward",
            Sort => "sort",
            Limit => "limit",
            Search => "search",
            SearchReverse => "search-reverse",
            SearchNext => "search-next",
            NextNew => "next-new",
            PrevNew => "previous-new",
            ChangeMailbox => "change-mailbox",
            ChangeMailboxReadOnly => "change-mailbox-readonly",
            Folders => "folders",
            Attachments => "attachments",
            FoldThread => "fold-thread",
            FoldAll => "fold-all",
            Print => "print",
            Tag => "tag",
            TagPrefix => "tag-prefix",
            DeleteThread => "delete-thread",
            UndeleteThread => "undelete-thread",
            TagThread => "tag-thread",
            DeleteSubthread => "delete-subthread",
            UndeleteSubthread => "undelete-subthread",
            NextThread => "next-thread",
            BreakThread => "break-thread",
            LinkThreads => "link-threads",
            ReadThread => "read-thread",
            ReadSubthread => "read-subthread",
            TagSubthread => "tag-subthread",
            ParentMessage => "parent-message",
            RootMessage => "root-message",
            EditLabel => "edit-label",
            ShowVersion => "show-version",
            ShowLimit => "show-limit",
            ToggleWrite => "toggle-write",
            DisplayAddress => "display-address",
            PageTop => "top-page",
            PageMiddle => "middle-page",
            PageBottom => "bottom-page",
            PrevThread => "previous-thread",
            Undo => "undo",
            DeletePattern => "delete-pattern",
            UndeletePattern => "undelete-pattern",
            TagPattern => "tag-pattern",
            UntagPattern => "untag-pattern",
            FetchMail => "fetch-mail",
            Save => "save",
            Copy => "copy",
            DecodeSave => "decode-save",
            DecodeCopy => "decode-copy",
            Pipe => "pipe",
            Bounce => "bounce",
            Resend => "resend",
            Edit => "edit",
            SidebarToggle => "sidebar-toggle",
            SidebarNext => "sidebar-next",
            SidebarPrev => "sidebar-prev",
            SidebarOpen => "sidebar-open",
            CreateAlias => "create-alias",
            Query => "query",
            Notmuch => "notmuch",
            EnterCommand => "enter-command",
            Shell => "shell-escape",
            Redraw => "refresh",
            Suspend => "suspend",
            Help => "help",
        }
    }

    pub fn describe(self) -> &'static str {
        use Function::*;
        match self {
            Quit => "quit (writes changes; asks before purging deletions)",
            Abort => "quit without saving changes",
            Down => "next message",
            Up => "previous message",
            PageDown => "page down",
            PageUp => "page up",
            First => "first message",
            Last => "last message",
            View => "view message",
            Delete => "mark for deletion",
            Undelete => "unmark deletion",
            Flag => "toggle flagged mark",
            ToggleNew => "toggle read/unread",
            Sync => "write changes to the maildir",
            Compose => "compose a new message",
            Reply => "reply to sender",
            GroupReply => "reply to all",
            ListReply => "reply to the mailing list only",
            Forward => "forward message",
            Sort => "choose sort order",
            Limit => "limit index by pattern",
            Search => "search messages by pattern (the pager / searches its text)",
            SearchReverse => "search backwards; n then repeats backwards too",
            SearchNext => "repeat last search, the way it was going",
            NextNew => "jump to the next new or unread message",
            PrevNew => "jump to the previous new or unread message",
            ChangeMailbox => "open a mailbox by path",
            ChangeMailboxReadOnly => "open a mailbox read-only (Alt+c)",
            Folders => "browse nearby mailboxes",
            Attachments => "list message parts",
            FoldThread => "fold/unfold current thread",
            FoldAll => "fold/unfold all threads",
            Print => "pipe message to the print command",
            Tag => "toggle the tag on this message",
            TagPrefix => "apply the next function to tagged messages",
            DeleteThread => "mark the whole thread for deletion",
            UndeleteThread => "unmark the whole thread",
            TagThread => "tag/untag the whole thread",
            DeleteSubthread => "mark this message and its replies for deletion",
            UndeleteSubthread => "unmark this message and its replies",
            NextThread => "jump to the next thread",
            BreakThread => "break the thread in two at this message",
            LinkThreads => "link the tagged messages under this one",
            ReadThread => "mark the whole thread read",
            ReadSubthread => "mark this message and its replies read",
            TagSubthread => "tag this message and its replies",
            ParentMessage => "jump to the parent message",
            RootMessage => "jump to the thread's root message",
            EditLabel => "add, change or clear the X-Label",
            ShowVersion => "show the rmut version",
            ShowLimit => "show the active limit pattern",
            ToggleWrite => "toggle the mailbox's read-only state",
            DisplayAddress => "show the sender's full address",
            PageTop => "move to the top of the page",
            PageMiddle => "move to the middle of the page",
            PageBottom => "move to the bottom of the page",
            PrevThread => "jump to the previous thread",
            Undo => "cancel a held send, else undo the last delete/flag/tag/save",
            DeletePattern => "delete every message matching a pattern",
            UndeletePattern => "undelete every message matching a pattern",
            TagPattern => "tag every message matching a pattern",
            UntagPattern => "untag every message matching a pattern",
            FetchMail => "check for new mail now",
            Save => "save (copy + mark deleted) to a mailbox",
            DecodeSave => "decode-save: the decoded message, original deleted",
            DecodeCopy => "decode-copy: the decoded message",
            Copy => "copy to a mailbox (original stays)",
            Pipe => "pipe raw message to a shell command",
            Bounce => "bounce (resend) message to new recipients",
            Resend => "edit the message as a new draft",
            Edit => "edit the raw message and replace it",
            SidebarToggle => "show/hide the mailbox sidebar",
            SidebarNext => "highlight the next sidebar mailbox",
            SidebarPrev => "highlight the previous sidebar mailbox",
            SidebarOpen => "open the highlighted sidebar mailbox",
            CreateAlias => "add the sender to the alias file",
            Query => "look up addresses with query_command",
            Notmuch => "notmuch search into a read-only view",
            EnterCommand => "run a config command (set/bind/macro/color/...)",
            Shell => "run a shell command",
            Redraw => "repaint the screen",
            Suspend => "suspend rmut (fg brings it back)",
            Help => "this help",
        }
    }

    /// Every function, in the order the help screen and a menu bar
    /// want them.
    pub fn all() -> &'static [Function] {
        use Function::*;
        &[
            Quit,
            Abort,
            Down,
            Up,
            PageDown,
            PageUp,
            First,
            Last,
            View,
            Delete,
            Undelete,
            Flag,
            ToggleNew,
            Sync,
            Compose,
            Reply,
            GroupReply,
            ListReply,
            Forward,
            Sort,
            Limit,
            Search,
            SearchReverse,
            SearchNext,
            NextNew,
            PrevNew,
            ChangeMailbox,
            ChangeMailboxReadOnly,
            Folders,
            Attachments,
            FoldThread,
            FoldAll,
            Print,
            Tag,
            TagPrefix,
            DeleteThread,
            UndeleteThread,
            TagThread,
            DeleteSubthread,
            UndeleteSubthread,
            NextThread,
            PrevThread,
            BreakThread,
            LinkThreads,
            ReadThread,
            ReadSubthread,
            TagSubthread,
            ParentMessage,
            RootMessage,
            EditLabel,
            ShowVersion,
            ShowLimit,
            ToggleWrite,
            DisplayAddress,
            PageTop,
            PageMiddle,
            PageBottom,
            Undo,
            DeletePattern,
            UndeletePattern,
            TagPattern,
            UntagPattern,
            FetchMail,
            Save,
            Copy,
            DecodeSave,
            DecodeCopy,
            Pipe,
            Bounce,
            Resend,
            Edit,
            SidebarToggle,
            SidebarNext,
            SidebarPrev,
            SidebarOpen,
            CreateAlias,
            Query,
            Notmuch,
            EnterCommand,
            Shell,
            Redraw,
            Suspend,
            Help,
        ]
    }

    pub fn from_name(name: &str) -> Option<Function> {
        Function::all().iter().copied().find(|a| a.name() == name)
    }

    /// Which functions `;` (tag-prefix) can hand the tagged set to.
    /// The rest say so rather than quietly acting on one message:
    /// resend and edit open a draft or an editor, of which rmut has
    /// one at a time.
    pub fn takes_tagged(self) -> bool {
        use Function::*;
        matches!(
            self,
            Delete
                | Undelete
                | Flag
                | ToggleNew
                | Tag
                | Save
                | Copy
                | DecodeSave
                | DecodeCopy
                | Pipe
                | Print
                | Bounce
                | EditLabel
        )
    }
}

/// What came of running a function.
pub enum Outcome {
    /// The session did it. Whatever there was to say went out as a
    /// [`Notice`](rmut_core::notice::Notice); whatever the front end
    /// must do next is waiting in [`Session::take_request`].
    Done,
    /// It cannot finish without an answer. The front end collects one
    /// however it likes and hands it back to [`Session::answer`].
    Ask(Ask),
    /// Only the front end can do this one.
    Front(FrontOp),
}

impl From<Option<Ask>> for Outcome {
    /// The `ask_*` helpers return `None` when the question does not
    /// arise (nothing to act on, a read-only mailbox), having already
    /// said why.
    fn from(ask: Option<Ask>) -> Outcome {
        match ask {
            Some(ask) => Outcome::Ask(ask),
            None => Outcome::Done,
        }
    }
}

/// The functions a session cannot carry out, because they are about
/// the display rather than the mail: a menu to open, a screen to
/// repaint, an editor to hand the terminal to. The session has done
/// whatever checking it can (a mailbox with unsaved changes will not
/// be left, an unconfigured query will not be prompted for) before
/// handing one of these back, so a front end can act on it directly.
pub enum FrontOp {
    /// mutt's `x`: leave without writing anything back.
    Exit,
    /// Read the selected message, however messages are read.
    OpenSelected,
    /// Start a draft. The session asks for the recipients once the
    /// front end has settled `$recall`.
    Compose(ComposeKind),
    /// mutt's resend-message: the selected message as a new draft.
    Resend,
    /// mutt's `e`: the selected message's own bytes, in an editor.
    RawEdit,
    /// The attachment menu for the selected message.
    Attachments,
    /// The folder browser.
    Folders,
    /// `$query_command`, which is configured: ask for the terms.
    Query,
    /// notmuch, which is not disabled: ask for the query.
    Notmuch,
    /// Somewhere else to open; the open mailbox is ready to be left.
    ChangeMailbox { read_only: bool },
    /// The `:` command line.
    CommandPrompt,
    /// mutt's `;`: the next function applies to the tagged set. There
    /// are tagged messages, or this would have been a complaint.
    TagPrefix,
    /// The mailbox pane.
    Sidebar(SidebarOp),
    /// Move the cursor by where it sits on screen, which only the
    /// front end knows: mutt's H, M and L.
    PageMove(PageSpot),
    /// mutt's help screen.
    Help,
    /// mutt's Ctrl+L: repaint.
    Redraw,
}

/// What to do with the mailbox pane.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SidebarOp {
    Toggle,
    Next,
    Prev,
    Open,
}

/// Where on the visible page the cursor should land.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageSpot {
    Top,
    Middle,
    Bottom,
}

impl Session {
    /// Do one thing, whatever asked for it: a key, a macro replay, a
    /// `:exec`, a menu item.
    ///
    /// `tagged` is mutt's tag-prefix, and is only ever true for a
    /// function that [takes it](Function::takes_tagged); `page` is how
    /// many messages the front end is showing at once, which is all
    /// the geometry the session needs to know.
    pub fn run_function(&mut self, function: Function, tagged: bool, page: usize) -> Outcome {
        use Function::*;
        match function {
            // ---- motion ----
            Down => self.select(self.sel.saturating_add(1)),
            Up => self.select(self.sel.saturating_sub(1)),
            PageDown => self.select(self.sel.saturating_add(page)),
            PageUp => self.select(self.sel.saturating_sub(page)),
            First => self.select(0),
            Last => self.select(usize::MAX),
            NextNew => self.jump_new(true),
            PrevNew => self.jump_new(false),
            NextThread => self.jump_thread(true),
            PrevThread => self.jump_thread(false),
            ParentMessage => self.jump_parent(false),
            RootMessage => self.jump_parent(true),
            SearchNext => self.search_next(),

            // ---- marks ----
            Tag => {
                if let Some(&i) = self.visible.get(self.sel) {
                    self.push_undo("tag", &[i]);
                    self.msgs[i].env.tagged = !self.msgs[i].env.tagged;
                    self.select(self.sel.saturating_add(1));
                }
            }
            Delete => {
                let rules = self.delete_rules();
                self.mark_selected(tagged, "delete", move |m| {
                    rules.mark(m);
                })
            }
            Undelete => {
                self.mark_selected(tagged, "undelete", |m| m.env.file.flags.deleted = false)
            }
            Flag => self.mark_selected(tagged, "flag", |m| {
                m.env.file.flags.flagged = !m.env.file.flags.flagged
            }),
            ToggleNew => self.mark_selected(tagged, "toggle read", |m| {
                m.env.file.flags.seen = !m.env.file.flags.seen;
                m.env.file.is_new = false;
            }),
            Undo => {
                // A message still inside its $undo_send window is the
                // most recent thing done, so it is what undo takes
                // back first.
                if !self.cancel_send() {
                    self.undo_last();
                }
            }

            // ---- threads ----
            DeleteThread => self.thread_mark(false, ThreadOp::Delete),
            UndeleteThread => self.thread_mark(false, ThreadOp::Undelete),
            TagThread => self.thread_mark(false, ThreadOp::Tag),
            ReadThread => self.thread_mark(false, ThreadOp::Read),
            DeleteSubthread => self.thread_mark(true, ThreadOp::Delete),
            UndeleteSubthread => self.thread_mark(true, ThreadOp::Undelete),
            TagSubthread => self.thread_mark(true, ThreadOp::Tag),
            ReadSubthread => self.thread_mark(true, ThreadOp::Read),
            BreakThread => self.break_thread(),
            LinkThreads => self.link_threads(),
            FoldThread => self.toggle_collapse(false),
            FoldAll => self.toggle_collapse(true),

            // ---- the mailbox ----
            Sync => {
                if self.deleted_count() > 0 {
                    return self.ask_purge(false).into();
                }
                self.sync(true);
            }
            Quit => return self.leave().into(),
            FetchMail => {
                self.check_new_mail();
                if self.notice().is_none() {
                    self.note("checked for new mail");
                }
            }
            ToggleWrite => self.toggle_write(),
            ShowLimit => self.show_limit(),
            ShowVersion => self.note(concat!("rmut ", env!("CARGO_PKG_VERSION"))),
            DisplayAddress => {
                let from = self
                    .visible
                    .get(self.sel)
                    .map(|&i| self.msgs[i].env.from_full.clone());
                match from {
                    Some(from) if !from.trim().is_empty() => self.note(from),
                    _ => self.note("(no From address)"),
                }
            }

            // ---- questions ----
            Limit => return Outcome::Ask(self.ask_limit()),
            Search => return Outcome::Ask(self.ask_search(false)),
            SearchReverse => return Outcome::Ask(self.ask_search(true)),
            Sort => return Outcome::Ask(self.ask_sort()),
            Shell => return Outcome::Ask(self.ask_shell()),
            DeletePattern => return self.ask_pattern(PatternOp::Delete).into(),
            UndeletePattern => return self.ask_pattern(PatternOp::Undelete).into(),
            TagPattern => return self.ask_pattern(PatternOp::Tag).into(),
            UntagPattern => return self.ask_pattern(PatternOp::Untag).into(),
            Save => return self.ask_copy(true, tagged).into(),
            Copy => return self.ask_copy(false, tagged).into(),
            DecodeSave => return self.ask_copy_decode(true, tagged, true).into(),
            DecodeCopy => return self.ask_copy_decode(false, tagged, true).into(),
            Pipe => return self.ask_pipe(tagged).into(),
            Bounce => return self.ask_bounce(tagged).into(),
            Print => return self.ask_print(tagged).into(),
            EditLabel => return self.ask_edit_label(tagged).into(),
            CreateAlias => return self.ask_alias().into(),
            ListReply => return self.start_list_reply().into(),

            // ---- off to the front end ----
            Suspend => self.request_suspend(),
            Abort => return Outcome::Front(FrontOp::Exit),
            View => return Outcome::Front(FrontOp::OpenSelected),
            Compose => return Outcome::Front(FrontOp::Compose(ComposeKind::New)),
            Reply => return Outcome::Front(FrontOp::Compose(ComposeKind::Reply)),
            GroupReply => return Outcome::Front(FrontOp::Compose(ComposeKind::GroupReply)),
            Forward => return Outcome::Front(FrontOp::Compose(ComposeKind::Forward)),
            Resend => return Outcome::Front(FrontOp::Resend),
            Edit => return Outcome::Front(FrontOp::RawEdit),
            Attachments => return Outcome::Front(FrontOp::Attachments),
            Folders => return Outcome::Front(FrontOp::Folders),
            EnterCommand => return Outcome::Front(FrontOp::CommandPrompt),
            Help => return Outcome::Front(FrontOp::Help),
            Redraw => return Outcome::Front(FrontOp::Redraw),
            PageTop => return Outcome::Front(FrontOp::PageMove(PageSpot::Top)),
            PageMiddle => return Outcome::Front(FrontOp::PageMove(PageSpot::Middle)),
            PageBottom => return Outcome::Front(FrontOp::PageMove(PageSpot::Bottom)),
            SidebarToggle => return Outcome::Front(FrontOp::Sidebar(SidebarOp::Toggle)),
            SidebarNext => return Outcome::Front(FrontOp::Sidebar(SidebarOp::Next)),
            SidebarPrev => return Outcome::Front(FrontOp::Sidebar(SidebarOp::Prev)),
            SidebarOpen => return Outcome::Front(FrontOp::Sidebar(SidebarOp::Open)),
            TagPrefix => {
                if !self.msgs.iter().any(|m| m.env.tagged) {
                    self.note("no tagged messages");
                    return Outcome::Done;
                }
                // mutt writes "Tag-" on its message line and waits;
                // rmut's one bottom line appends it to the status bar,
                // which then stays readable.
                self.note("Tag-");
                return Outcome::Front(FrontOp::TagPrefix);
            }
            Query => {
                if self.config.mail.query_command.is_none() {
                    self.error("no query_command configured");
                    return Outcome::Done;
                }
                return Outcome::Front(FrontOp::Query);
            }
            Notmuch => {
                if self.config.mail.notmuch == Some(false) {
                    self.error("notmuch is disabled in the config");
                    return Outcome::Done;
                }
                return Outcome::Front(FrontOp::Notmuch);
            }
            ChangeMailbox | ChangeMailboxReadOnly => {
                if !self.ready_to_leave() {
                    return Outcome::Done;
                }
                return Outcome::Front(FrontOp::ChangeMailbox {
                    read_only: function == ChangeMailboxReadOnly,
                });
            }
        }
        Outcome::Done
    }

    /// One flag change, on the tagged set or on the message under the
    /// cursor. mutt's $resolve (on by default) advances afterwards.
    fn mark_selected(&mut self, tagged: bool, what: &'static str, f: impl Fn(&mut crate::Msg)) {
        if self.deny_readonly() {
            return;
        }
        if tagged {
            self.each_tagged(what, f);
        } else if let Some(&i) = self.visible.get(self.sel) {
            self.push_undo(what, &[i]);
            f(&mut self.msgs[i]);
            self.msgs[i].dirty = true;
            self.select(self.sel.saturating_add(1));
        }
    }
}
