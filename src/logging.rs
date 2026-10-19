//! Logging for the daemon and the CLI commands (never the TUI: it owns the terminal).
//!
//! The level comes from `--log-level`, else `NEIRO_LOG` (env_logger filter syntax,
//! e.g. `debug` or `info,neiro::system=trace`), else the caller's default. Under
//! systemd, stderr goes to the journal: lines then carry a `<N>` syslog priority
//! prefix and no timestamp, so `journalctl -p` filters by level.

use log::Level;
use std::io::Write;

pub const ENV: &str = "NEIRO_LOG";

pub fn init(level: Option<&str>, default: &str) {
    let filter = level.map(String::from).or_else(|| std::env::var(ENV).ok()).unwrap_or_else(|| default.into());
    let mut b = env_logger::Builder::new();
    b.parse_filters(&filter);
    if stderr_is_journal() {
        b.format(|buf, rec| {
            let prio = match rec.level() {
                Level::Error => 3,
                Level::Warn => 4,
                Level::Info => 6,
                Level::Debug | Level::Trace => 7,
            };
            writeln!(buf, "<{prio}>{}", rec.args())
        });
    } else {
        b.format_timestamp_millis().format_target(false);
    }
    let _ = b.try_init();
}

/// systemd sets `JOURNAL_STREAM=<dev>:<inode>` for a service whose stderr is the
/// journal; it's inherited by children, so compare it with our actual stderr.
fn stderr_is_journal() -> bool {
    let Some((dev, ino)) = std::env::var("JOURNAL_STREAM").ok().and_then(|v| {
        let (d, i) = v.split_once(':')?;
        Some((d.parse::<u64>().ok()?, i.parse::<u64>().ok()?))
    }) else {
        return false;
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(2, &mut st) } != 0 {
        return false;
    }
    st.st_dev as u64 == dev && st.st_ino as u64 == ino
}
