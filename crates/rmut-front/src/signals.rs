//! The signals that end rmut from outside: SIGTERM (a kill, a
//! logout) and SIGHUP (the terminal window closed). Their default is
//! to die on the spot, which dropped the mail held for $undo_send and
//! the sync on the way out. The handler only notes which came; the
//! front end's loop sees it and leaves by the usual way out. A second
//! one while that way out is still busy ends the process at once.
//! The terminal leaves SIGINT as it was (in raw mode Ctrl+C is a
//! key); the window takes it too.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Which signal asked rmut to go.
static CAUGHT: AtomicI32 = AtomicI32::new(0);
/// Whether one came already: a second ends the process.
static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn note(sig: libc::c_int) {
    if SIGNALLED.swap(true, Ordering::SeqCst) {
        // Asked twice: whatever the way out is waiting on, stop.
        unsafe { libc::_exit(128 + sig) };
    }
    let _ = CAUGHT.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
    if sig == libc::SIGHUP {
        // The terminal is gone, and reading it gives end-of-file for
        // ever, which crossterm 0.28 reads in a loop that never ends.
        // An empty pipe in its place says "nothing yet" instead, so
        // the wait returns and the loop sees the hangup. pipe2 and
        // dup2 are safe in a handler; the write end stays open.
        unsafe {
            let mut fds = [0; 2];
            if libc::pipe2(fds.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) == 0 {
                libc::dup2(fds[0], 0);
            }
        }
    }
}

/// Catch SIGTERM and SIGHUP for [`caught`]. Without SA_RESTART, so a
/// wait for input wakes up to look.
pub fn install() {
    catch(&[libc::SIGTERM, libc::SIGHUP]);
}

/// [`install`], and SIGINT too: for the window, whose launching
/// terminal stays in cooked mode, so Ctrl+C there is a signal rather
/// than a key.
pub fn install_with_interrupt() {
    catch(&[libc::SIGTERM, libc::SIGHUP, libc::SIGINT]);
}

fn catch(signals: &[libc::c_int]) {
    for &sig in signals {
        unsafe {
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = note as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigemptyset(&mut act.sa_mask);
            libc::sigaction(sig, &act, std::ptr::null_mut());
        }
    }
}

/// Whether a signal asked rmut to go.
pub fn caught() -> bool {
    CAUGHT.load(Ordering::SeqCst) != 0
}

/// Whether it was SIGHUP: the terminal is gone, and writing to it
/// (restoring it, printing a last note) fails or worse.
pub fn hung_up() -> bool {
    CAUGHT.load(Ordering::SeqCst) == libc::SIGHUP
}
