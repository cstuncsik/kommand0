use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a short, unique id from the millisecond timestamp, the process id,
/// and a monotonic per-process counter. The counter keeps ids unique for several
/// creates within one millisecond in a single process; the process id keeps two
/// processes (e.g. the `kmd` CLI and a running TUI) from colliding when they mint
/// ids in the same millisecond. `duration_since` falls back to 0 on a
/// backwards/stuck clock rather than panicking.
pub fn generate_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{millis:x}-{:x}-{seq:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_carry_the_pid() {
        let a = generate_id();
        let b = generate_id();
        assert_ne!(a, b, "consecutive ids differ (the per-process counter)");
        let pid = format!("{:x}", std::process::id());
        assert!(a.contains(&format!("-{pid}-")), "id embeds the pid for cross-process uniqueness: {a}");
        assert_eq!(a.matches('-').count(), 2, "millis-pid-seq shape: {a}");
    }
}
