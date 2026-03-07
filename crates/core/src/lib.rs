use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AppState {
    pub sessions: Vec<Session>,
    pub running: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub id: u64,
    pub name: String,
}
