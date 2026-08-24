pub mod alias;
pub mod command;
pub mod compose;
pub mod config;
pub mod flowed;
pub mod format;
pub mod hdrcache;
pub mod imap;
pub mod maildir;
pub mod mailto;
pub mod mbox;
pub mod message;
pub mod muttrc;
mod net;
pub mod pattern;
pub mod pgp;
pub mod remote;
pub mod smtp;
pub mod thread;

#[cfg(test)]
mod testserver;
