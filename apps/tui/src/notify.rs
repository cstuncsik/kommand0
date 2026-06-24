//! Attention notifications: ring the terminal bell and/or raise an OS
//! notification when a backgrounded session goes quiet with unseen output.
//!
//! The *decision* (mode parsing, command building) lives here as pure functions
//! so it can be unit-tested; the *doing* (writing the bell byte, spawning the
//! notifier) is a thin layer in `main.rs` that calls into this.

use std::io::Write;

/// How to alert when a backgrounded session needs you. Parsed from the `notify`
/// config field; defaults to [`NotifyMode::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum NotifyMode {
    #[default]
    Off,
    Bell,
    Desktop,
    Both,
}

impl NotifyMode {
    /// Parse the `notify` config value. Unknown values fall back to `Off` with a
    /// warning (mirroring how an unknown theme name degrades).
    pub(crate) fn parse(value: Option<&str>) -> (Self, Option<String>) {
        let Some(v) = value else {
            return (Self::Off, None);
        };
        match v.trim().to_ascii_lowercase().as_str() {
            "" | "off" => (Self::Off, None),
            "bell" => (Self::Bell, None),
            "desktop" => (Self::Desktop, None),
            "both" => (Self::Both, None),
            other => (
                Self::Off,
                Some(format!(
                    "unknown notify mode {other:?} (use off/bell/desktop/both); notifications off"
                )),
            ),
        }
    }

    pub(crate) fn wants_bell(self) -> bool {
        matches!(self, Self::Bell | Self::Both)
    }

    pub(crate) fn wants_desktop(self) -> bool {
        matches!(self, Self::Desktop | Self::Both)
    }
}

/// Write a terminal bell (BEL) to `out`. The terminal decides whether to beep or
/// flash; BEL writes no cells, so it never disturbs the rendered screen.
pub(crate) fn ring_bell(out: &mut impl Write) {
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}

/// The OS-notifier command for the current platform as `(program, args)`, or
/// `None` if unsupported. On Linux `title`/`body` are passed as separate argv
/// (no shell, no escaping); on macOS they're embedded in an AppleScript string
/// with `\` and `"` escaped so a workspace name can't break out of the literal.
pub(crate) fn desktop_command(title: &str, body: &str) -> Option<(String, Vec<String>)> {
    #[cfg(target_os = "macos")]
    {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display notification \"{}\" with title \"{}\"",
            esc(body),
            esc(title)
        );
        Some(("osascript".to_string(), vec!["-e".to_string(), script]))
    }
    #[cfg(target_os = "linux")]
    {
        Some((
            "notify-send".to_string(),
            vec![title.to_string(), body.to_string()],
        ))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = (title, body);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_modes() {
        assert_eq!(NotifyMode::parse(None).0, NotifyMode::Off);
        assert_eq!(NotifyMode::parse(Some("off")).0, NotifyMode::Off);
        assert_eq!(NotifyMode::parse(Some("bell")).0, NotifyMode::Bell);
        assert_eq!(NotifyMode::parse(Some("desktop")).0, NotifyMode::Desktop);
        assert_eq!(NotifyMode::parse(Some("both")).0, NotifyMode::Both);
        assert_eq!(NotifyMode::parse(Some(" BoTh ")).0, NotifyMode::Both, "trimmed + case-insensitive");
        assert!(NotifyMode::parse(Some("bell")).1.is_none(), "valid value warns nothing");

        let (mode, warn) = NotifyMode::parse(Some("buzz"));
        assert_eq!(mode, NotifyMode::Off);
        assert!(warn.is_some_and(|w| w.contains("unknown notify mode")));
    }

    #[test]
    fn wants_flags() {
        assert!(!NotifyMode::Off.wants_bell() && !NotifyMode::Off.wants_desktop());
        assert!(NotifyMode::Bell.wants_bell() && !NotifyMode::Bell.wants_desktop());
        assert!(!NotifyMode::Desktop.wants_bell() && NotifyMode::Desktop.wants_desktop());
        assert!(NotifyMode::Both.wants_bell() && NotifyMode::Both.wants_desktop());
    }

    #[test]
    fn ring_bell_writes_only_bel() {
        let mut buf: Vec<u8> = Vec::new();
        ring_bell(&mut buf);
        assert_eq!(buf, b"\x07");
    }

    #[test]
    fn desktop_command_for_this_platform() {
        let cmd = desktop_command("kommand0", "demo-ws is waiting");
        #[cfg(target_os = "macos")]
        {
            let (prog, args) = cmd.expect("macOS uses osascript");
            assert_eq!(prog, "osascript");
            assert_eq!(args[0], "-e");
            assert!(args[1].contains("display notification"));
            assert!(args[1].contains("kommand0") && args[1].contains("demo-ws is waiting"));
        }
        #[cfg(target_os = "linux")]
        {
            let (prog, args) = cmd.expect("Linux uses notify-send");
            assert_eq!(prog, "notify-send");
            assert_eq!(args, vec!["kommand0".to_string(), "demo-ws is waiting".to_string()]);
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert!(cmd.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_escapes_quotes_in_applescript() {
        let (_, args) = desktop_command("kommand0", "weird \"name\"").unwrap();
        assert!(
            args[1].contains("\\\"name\\\""),
            "embedded quotes are escaped: {}",
            args[1]
        );
    }
}
