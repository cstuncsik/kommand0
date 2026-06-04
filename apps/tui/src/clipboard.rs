use arboard::Clipboard;

pub struct ClipboardBridge {
    clipboard: Option<Clipboard>,
}

impl ClipboardBridge {
    pub fn new() -> Self {
        Self {
            clipboard: Clipboard::new().ok(),
        }
    }

    pub fn is_available(&self) -> bool {
        self.clipboard.is_some()
    }

    pub fn set_text(&mut self, text: &str) -> Result<(), String> {
        match &mut self.clipboard {
            Some(cb) => cb.set_text(text.to_string()).map_err(|e| e.to_string()),
            None => Err("Clipboard not available".to_string()),
        }
    }

    pub fn get_text(&mut self) -> Result<String, String> {
        match &mut self.clipboard {
            Some(cb) => cb.get_text().map_err(|e| e.to_string()),
            None => Err("Clipboard not available".to_string()),
        }
    }

    #[cfg(test)]
    fn new_unavailable() -> Self {
        Self { clipboard: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_does_not_panic() {
        let _bridge = ClipboardBridge::new();
    }

    #[test]
    fn is_available_returns_bool() {
        let bridge = ClipboardBridge::new();
        let _available: bool = bridge.is_available();
    }

    #[test]
    fn set_text_unavailable_returns_err() {
        let mut bridge = ClipboardBridge::new_unavailable();
        assert!(!bridge.is_available());
        let result = bridge.set_text("test");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Clipboard not available");
    }

    #[test]
    #[ignore]
    fn set_text_succeeds_on_real_system() {
        let mut bridge = ClipboardBridge::new();
        if bridge.is_available() {
            let result = bridge.set_text("dalat clipboard test");
            assert!(result.is_ok(), "set_text failed: {:?}", result.err());
        }
    }

    #[test]
    fn get_text_unavailable_returns_err() {
        let mut bridge = ClipboardBridge::new_unavailable();
        assert!(!bridge.is_available());
        let result = bridge.get_text();
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Clipboard not available");
    }

    #[test]
    #[ignore]
    fn get_text_round_trips_on_real_system() {
        let mut bridge = ClipboardBridge::new();
        if bridge.is_available() {
            let _ = bridge.set_text("dalat paste test");
            let result = bridge.get_text();
            assert!(result.is_ok(), "get_text failed: {:?}", result.err());
        }
    }
}
