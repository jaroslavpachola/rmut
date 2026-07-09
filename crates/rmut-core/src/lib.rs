pub mod alias;
pub mod compose;
pub mod config;
pub mod format;
pub mod imap;
pub mod maildir;
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
