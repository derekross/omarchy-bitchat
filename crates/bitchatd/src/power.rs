//! Radio duty cycle: how hard to scan, from the user's mode plus what the
//! laptop is doing (on battery, Bluetooth audio playing).

use std::path::Path;
use std::time::Duration;

use serde::Serialize;

use crate::store::Mode;

/// What the radio actually does right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Effective {
    Balanced,
    Saver,
    Off,
}

impl Effective {
    /// Scan on / off durations, from bitchat-android's `AppConstants.Power`.
    pub fn scan_cycle(self) -> Option<(Duration, Duration)> {
        match self {
            Effective::Balanced => Some((Duration::from_secs(8), Duration::from_secs(2))),
            Effective::Saver => Some((Duration::from_secs(2), Duration::from_secs(28))),
            Effective::Off => None,
        }
    }
}

pub fn resolve(mode: Mode, on_battery: bool, audio_active: bool) -> Effective {
    match mode {
        Mode::Off => Effective::Off,
        Mode::Balanced => Effective::Balanced,
        Mode::Saver => Effective::Saver,
        Mode::Auto if on_battery || audio_active => Effective::Saver,
        Mode::Auto => Effective::Balanced,
    }
}

/// True when mains power supplies exist and none is online. A machine
/// with no mains supply listed (a desktop) counts as on AC.
pub fn on_battery() -> bool {
    on_battery_in(Path::new("/sys/class/power_supply"))
}

fn on_battery_in(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut saw_mains = false;
    for entry in entries.flatten() {
        let path = entry.path();
        let kind = std::fs::read_to_string(path.join("type")).unwrap_or_default();
        if kind.trim() != "Mains" {
            continue;
        }
        saw_mains = true;
        if std::fs::read_to_string(path.join("online")).unwrap_or_default().trim() == "1" {
            return false;
        }
    }
    saw_mains
}

/// A2DP sink: the UUID headphones and speakers expose.
pub const AUDIO_SINK_UUID: &str = "0000110b-0000-1000-8000-00805f9b34fb";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution() {
        assert_eq!(resolve(Mode::Auto, false, false), Effective::Balanced);
        assert_eq!(resolve(Mode::Auto, true, false), Effective::Saver);
        assert_eq!(resolve(Mode::Auto, false, true), Effective::Saver);
        assert_eq!(resolve(Mode::Balanced, true, true), Effective::Balanced);
        assert_eq!(resolve(Mode::Off, false, false), Effective::Off);
        assert!(Effective::Off.scan_cycle().is_none());
    }

    #[test]
    fn battery_detection() {
        let base = std::env::temp_dir().join(format!("bitchatd-ps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("BAT0")).unwrap();
        std::fs::write(base.join("BAT0/type"), "Battery\n").unwrap();
        assert!(!on_battery_in(&base), "no mains listed = desktop = AC");

        std::fs::create_dir_all(base.join("AC")).unwrap();
        std::fs::write(base.join("AC/type"), "Mains\n").unwrap();
        std::fs::write(base.join("AC/online"), "0\n").unwrap();
        assert!(on_battery_in(&base));
        std::fs::write(base.join("AC/online"), "1\n").unwrap();
        assert!(!on_battery_in(&base));
    }
}
