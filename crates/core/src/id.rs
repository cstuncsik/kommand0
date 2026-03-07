use std::time::{SystemTime, UNIX_EPOCH};

pub fn generate_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_millis();
    format!("{:x}", millis)
}
