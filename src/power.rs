//! Power actions exposed through the launcher power menu.

use std::process::Command;

#[derive(Clone, Copy)]
pub enum PowerAction {
    Shutdown,
    Restart,
    Sleep,
}

impl PowerAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::Shutdown => "Shutdown",
            Self::Restart => "Restart",
            Self::Sleep => "Sleep",
        }
    }

    pub fn run(self) {
        let result = match self {
            Self::Shutdown => Command::new("shutdown.exe").args(["/s", "/t", "0"]).spawn(),
            Self::Restart => Command::new("shutdown.exe").args(["/r", "/t", "0"]).spawn(),
            Self::Sleep => Command::new("rundll32.exe")
                .args(["powrprof.dll,SetSuspendState", "0", "1", "0"])
                .spawn(),
        };
        if let Err(e) = result {
            crate::state::log(&format!("power action failed: {e}"));
        }
    }
}

pub const ACTIONS: [PowerAction; 3] = [
    PowerAction::Shutdown,
    PowerAction::Restart,
    PowerAction::Sleep,
];
