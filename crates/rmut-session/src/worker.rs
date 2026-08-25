//! The IMAP connection, on a thread of its own.
//!
//! An IMAP conversation is one command at a time over one socket, and
//! every one of them can take as long as the network feels like. Run
//! on the thread that draws the screen, that is a freeze: no keys
//! read, nothing repainted, no way out. So the connection lives here
//! instead, behind a channel: the session sends a [`Job`], the thread
//! runs it, and the answer comes back as a [`Done`].
//!
//! What the session needs to know cheaply and constantly (which
//! account, which folder, where the cache is) is not down the
//! channel; it is in [`Facts`], kept beside the handle.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Result, anyhow};
use rmut_core::config::Account;
use rmut_core::maildir::Flags;
use rmut_core::remote::{Progress, Remote};

/// What the session asks the connection to do.
pub enum Job {
    /// Complete the cached messages whose files hold headers only.
    FetchBodies(Vec<PathBuf>),
    /// A `$` sync: flag changes first, then the purge.
    Sync {
        flags: Vec<(PathBuf, Flags)>,
        deletes: Vec<PathBuf>,
    },
    /// $trash: UID COPY before a purge.
    CopyToFolder {
        paths: Vec<PathBuf>,
        mailbox: String,
    },
    /// APPEND: into a named folder, or into the account's Sent when
    /// no name is given (an Fcc).
    Append {
        mailbox: Option<String>,
        flags: Flags,
        body: Vec<u8>,
    },
    /// The poll tick: has anything happened on the server?
    CheckNew,
    /// The folder browser's list, with UNSEEN counts.
    Folders,
    /// One folder's UNSEEN count, for the sidebar.
    Unseen(String),
    /// Server-side `~b`: which UIDs hold this text.
    SearchBody(String),
    /// Another folder of the same account, on this connection.
    Switch(String),
}

impl Job {
    /// What to say while it runs, and in anything that goes wrong.
    pub fn what(&self) -> &'static str {
        match self {
            Job::FetchBodies(paths) => match paths.len() {
                1 => "fetching the message",
                _ => "fetching the messages",
            },
            Job::Sync { .. } => "syncing",
            Job::CopyToFolder { .. } => "copying to the trash",
            Job::Append { .. } => "saving to the server",
            Job::CheckNew => "checking for new mail",
            Job::Folders => "listing folders",
            Job::Unseen(_) => "counting unread",
            Job::SearchBody(_) => "searching on the server",
            Job::Switch(_) => "opening the folder",
        }
    }
}

/// What a job left behind.
pub enum Done {
    /// Nothing to report but success.
    Nothing,
    /// How many messages arrived (a check, or a switch).
    Arrived(usize),
    Folders(Vec<(String, usize)>),
    Unseen(usize),
    Uids(Vec<u32>),
    /// Where an append landed.
    Folder(String),
    /// A switch: the facts that came with the new folder. Boxed,
    /// since an Account is far bigger than a count.
    Switched(Box<Facts>),
}

/// What the session knows about the open folder without asking the
/// connection. Cheap, and unchanged while a job is in flight.
#[derive(Clone)]
pub struct Facts {
    /// `imap:account/mailbox`, for the status line and the browser.
    pub spec: String,
    pub account: Account,
    pub mailbox: String,
    pub cache: PathBuf,
    /// Older UIDs a huge folder left unfetched at open; the session
    /// hands them to the background backfill.
    pub pending_backfill: Vec<u32>,
}

impl Facts {
    fn of(remote: &Remote) -> Facts {
        Facts {
            spec: remote.spec.clone(),
            account: remote.account.clone(),
            mailbox: remote.mailbox.clone(),
            cache: remote.cache.clone(),
            pending_backfill: remote.pending_backfill.clone(),
        }
    }
}

/// A handle on the connection: the facts, and the thread doing the
/// talking.
pub struct Imap {
    pub facts: Facts,
    jobs: Sender<Job>,
    answers: Receiver<Result<Done>>,
    thread: Option<JoinHandle<()>>,
    /// What the connection last said it was doing, written from the
    /// thread and read whenever the session gets round to it.
    progress: Arc<Mutex<Option<String>>>,
    /// The job in flight, if any: what it is, for anyone drawing a
    /// status line.
    busy: Option<&'static str>,
}

impl Imap {
    /// Take an open connection onto a thread of its own.
    pub fn new(remote: Remote) -> Imap {
        let facts = Facts::of(&remote);
        let (jobs, inbox) = channel::<Job>();
        let (outbox, answers) = channel::<Result<Done>>();
        let progress = Arc::new(Mutex::new(None));
        let thread = std::thread::spawn({
            let progress = progress.clone();
            move || run(remote, inbox, outbox, progress)
        });
        Imap {
            facts,
            jobs,
            answers,
            thread: Some(thread),
            progress,
            busy: None,
        }
    }

    /// A progress callback that writes where [`Imap::progress`] can
    /// read it, for the open that happens before there is a thread.
    pub fn progress_sink(slot: &Arc<Mutex<Option<String>>>) -> Progress {
        let slot = slot.clone();
        Box::new(move |line: &str| {
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(line.to_string());
            }
        })
    }

    /// The last thing the connection said it was doing, taken.
    pub fn take_progress(&mut self) -> Option<String> {
        self.progress.lock().ok().and_then(|mut slot| slot.take())
    }

    /// What the connection is doing, if anything.
    pub fn busy(&self) -> Option<&'static str> {
        self.busy
    }

    /// Send a job off, to be collected later.
    pub fn start(&mut self, job: Job) -> Result<()> {
        let what = job.what();
        self.jobs
            .send(job)
            .map_err(|_| anyhow!("the connection is gone"))?;
        self.busy = Some(what);
        Ok(())
    }

    /// The answer, if the job is done; None while it is still running.
    pub fn collect(&mut self) -> Option<Result<Done>> {
        match self.answers.try_recv() {
            Ok(done) => {
                self.busy = None;
                Some(self.remember(done))
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.busy = None;
                Some(Err(anyhow!("the connection is gone")))
            }
        }
    }

    /// Run a job and wait for it, the way the operations did when they
    /// held the connection themselves. Every caller that learns to
    /// collect its answer later stops calling this.
    pub fn blocking(&mut self, job: Job) -> Result<Done> {
        self.start(job)?;
        self.wait()
    }

    /// Wait for the job in flight. The session uses this to settle
    /// what it started before asking for something else: one
    /// connection, one conversation.
    pub fn wait(&mut self) -> Result<Done> {
        let done = match self.answers.recv() {
            Ok(done) => done,
            Err(RecvError) => Err(anyhow!("the connection is gone")),
        };
        self.busy = None;
        self.remember(done)
    }

    /// A switch changes the facts; keep them in step.
    fn remember(&mut self, done: Result<Done>) -> Result<Done> {
        if let Ok(Done::Switched(facts)) = &done {
            self.facts = (**facts).clone();
        }
        done
    }

    /// The backfill takes the UIDs the open left over; they are only
    /// handed out once.
    pub fn take_backfill(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.facts.pending_backfill)
    }
}

impl Drop for Imap {
    fn drop(&mut self) {
        // Closing the channel ends the loop after whatever it is in
        // the middle of; the LOGOUT is the connection's own Drop.
        let (jobs, _) = channel();
        let _ = std::mem::replace(&mut self.jobs, jobs);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread: one job at a time, in the order they were asked for.
fn run(
    mut remote: Remote,
    jobs: Receiver<Job>,
    answers: Sender<Result<Done>>,
    progress: Arc<Mutex<Option<String>>>,
) {
    remote.set_progress(Imap::progress_sink(&progress));
    while let Ok(job) = jobs.recv() {
        let what = job.what();
        let done = do_job(&mut remote, job).map_err(|err| err.context(what));
        if let Ok(mut slot) = progress.lock() {
            *slot = None;
        }
        if answers.send(done).is_err() {
            break; // nobody is listening any more
        }
    }
}

fn do_job(remote: &mut Remote, job: Job) -> Result<Done> {
    match job {
        Job::FetchBodies(paths) => {
            for path in &paths {
                remote.fetch_body(path)?;
            }
            Ok(Done::Nothing)
        }
        Job::Sync { flags, deletes } => {
            for (path, flags) in &flags {
                remote.push_flags(path, *flags)?;
            }
            if !deletes.is_empty() {
                remote.delete(&deletes)?;
            }
            Ok(Done::Nothing)
        }
        Job::CopyToFolder { paths, mailbox } => {
            remote.copy_to_folder(&paths, &mailbox)?;
            Ok(Done::Nothing)
        }
        Job::Append {
            mailbox,
            flags,
            body,
        } => {
            let folder = match &mailbox {
                Some(mailbox) => remote.append_to(mailbox, flags, &body)?,
                None => remote.append_sent(&body)?,
            };
            Ok(Done::Folder(folder))
        }
        Job::CheckNew => Ok(Done::Arrived(remote.check_new()?)),
        Job::Folders => Ok(Done::Folders(remote.folders()?)),
        Job::Unseen(mailbox) => Ok(Done::Unseen(remote.unseen(&mailbox))),
        Job::SearchBody(term) => Ok(Done::Uids(remote.search_body(&term)?)),
        Job::Switch(mailbox) => {
            remote.switch(&mailbox)?;
            Ok(Done::Switched(Box::new(Facts::of(remote))))
        }
    }
}
