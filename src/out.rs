//! Printing that survives a closed stdout (for example `| head`).
use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static CRITICAL: AtomicBool = AtomicBool::new(false);
static CLOSED: AtomicBool = AtomicBool::new(false);

/// While on, a closed stdout does not end the process: a half-done delete is worse
/// than lost output. Printing then stays silent.
pub fn set_critical(on: bool) {
    CRITICAL.store(on, Ordering::SeqCst);
}

pub fn line(args: fmt::Arguments<'_>) {
    if CLOSED.load(Ordering::SeqCst) {
        return;
    }
    let mut so = io::stdout().lock();
    if let Err(e) = writeln!(so, "{args}") {
        if e.kind() == io::ErrorKind::BrokenPipe {
            if CRITICAL.load(Ordering::SeqCst) {
                CLOSED.store(true, Ordering::SeqCst);
            } else {
                std::process::exit(0);
            }
        }
    }
}

/// Like `println!`, but quiet on BrokenPipe.
macro_rules! outln {
    () => { $crate::out::line(format_args!("")) };
    ($($a:tt)*) => { $crate::out::line(format_args!($($a)*)) };
}
pub(crate) use outln;
