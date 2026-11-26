use std::process::Stdio;

use crate::constants::APP_NAME;
use crate::privilege::{InvokingUser, command_as_user};

#[derive(Debug, Clone, Copy)]
pub enum Urgency {
    Normal,
    Critical,
}

impl Urgency {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Critical => "critical",
        }
    }
}

/// Best-effort desktop notification sent as the invoking user via `notify-send`.
pub fn notify(user: &InvokingUser, title: &str, body: &str, urgency: Urgency) {
    let bus = format!("unix:path=/run/user/{}/bus", user.uid);
    let _ = command_as_user("notify-send", user)
        .env("DBUS_SESSION_BUS_ADDRESS", bus)
        .args(["-a", APP_NAME, "-u", urgency.as_str(), "--", title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}
