//! Logging that behaves differently under systemd than in a terminal.
//!
//! No logging crate: journald reads a `<N>` priority prefix off stderr, which
//! is about twenty lines to implement and saves a dependency in every binary.

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl Level {
    fn parse(raw: &str) -> Level {
        match raw.to_lowercase().as_str() {
            "debug" | "trace" => Level::Debug,
            "warn" | "warning" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }

    /// syslog priority, which is what journald reads off the stream.
    fn priority(self) -> &'static str {
        match self {
            Level::Debug => "<7>",
            Level::Info => "<6>",
            Level::Warn => "<4>",
            Level::Error => "<3>",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

static THRESHOLD: AtomicU8 = AtomicU8::new(Level::Info as u8);
static UNDER_SYSTEMD: AtomicU8 = AtomicU8::new(2); // 2 = not yet determined

pub fn init(level: &str) {
    THRESHOLD.store(Level::parse(level) as u8, Ordering::Relaxed);
    let journal = std::env::var_os("JOURNAL_STREAM").is_some();
    UNDER_SYSTEMD.store(u8::from(journal), Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 >= THRESHOLD.load(Ordering::Relaxed)
}

pub fn emit(level: Level, target: &str, message: &str) {
    if !enabled(level) {
        return;
    }
    let mut stderr = std::io::stderr().lock();
    let under_systemd = UNDER_SYSTEMD.load(Ordering::Relaxed) == 1;
    // A failed log write must never take the process down; a full pipe or a
    // closed stderr is not a reason to stop doing the actual work.
    let _ = if under_systemd {
        writeln!(stderr, "{}{target}: {message}", level.priority())
    } else {
        writeln!(stderr, "{:>5} {target}: {message}", level.label())
    };
}

#[macro_export]
macro_rules! log_at {
    ($level:expr, $target:expr, $($arg:tt)*) => {
        if $crate::log::enabled($level) {
            $crate::log::emit($level, $target, &format!($($arg)*));
        }
    };
}

#[macro_export]
macro_rules! debug {
    ($target:expr, $($arg:tt)*) => { $crate::log_at!($crate::log::Level::Debug, $target, $($arg)*) };
}
#[macro_export]
macro_rules! info {
    ($target:expr, $($arg:tt)*) => { $crate::log_at!($crate::log::Level::Info, $target, $($arg)*) };
}
#[macro_export]
macro_rules! warn {
    ($target:expr, $($arg:tt)*) => { $crate::log_at!($crate::log::Level::Warn, $target, $($arg)*) };
}
#[macro_export]
macro_rules! error {
    ($target:expr, $($arg:tt)*) => { $crate::log_at!($crate::log::Level::Error, $target, $($arg)*) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_order_by_severity() {
        assert!(Level::Error > Level::Warn);
        assert!(Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug);
    }

    #[test]
    fn unknown_level_names_fall_back_to_info() {
        assert_eq!(Level::parse("nonsense"), Level::Info);
        assert_eq!(Level::parse("DEBUG"), Level::Debug);
        assert_eq!(Level::parse("warning"), Level::Warn);
    }

    #[test]
    fn threshold_filters_lower_levels() {
        init("warn");
        assert!(!enabled(Level::Info));
        assert!(enabled(Level::Warn));
        assert!(enabled(Level::Error));
        init("info");
    }
}
