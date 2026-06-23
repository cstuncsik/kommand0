use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Generate a short, unique id. A monotonic per-process counter is appended to
/// the millisecond timestamp so ids stay unique even for several creates within
/// the same millisecond, or on a backwards/stuck clock (where the timestamp part
/// would otherwise repeat — `duration_since` falls back to 0 rather than panic).
pub fn generate_id() -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{millis:x}-{seq:x}")
}
