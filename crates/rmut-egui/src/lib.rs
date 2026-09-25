//! rmut in a window, as a library: the same session, keys and muttrc
//! as the terminal build, painted by egui. The `rmut-egui` binary is a
//! window of rmut's own around it; a host with a window of its own - a
//! desktop that puts apps in panes - drives the same [`Boot`] through
//! [`Boot::frame_hosted`], which leaves the window-wide things (zoom,
//! egui's focus, the visuals, which keys go where) to the host.
//!
//! `rmut_core` and `rmut_session` are re-exported so a host names the
//! same types this crate was built against.

pub mod app;
pub mod boot;
mod input;
mod nvim;
mod paint;
#[cfg(test)]
mod tests;

pub use boot::{Boot, OpenEvent, Plan};
pub use rmut_core;
pub use rmut_session;
