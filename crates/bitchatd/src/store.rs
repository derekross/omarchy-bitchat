//! On-disk state: identity (keys + nickname), settings, and message history.
//!
//! - `$XDG_DATA_HOME/omarchy-bitchat/identity.json` (0600): who we are.
//! - `$XDG_STATE_HOME/omarchy-bitchat/settings.json`: radio mode, history.
//! - `$XDG_STATE_HOME/omarchy-bitchat/messages.jsonl`: recent public chat.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use bitchat_proto::Identity;
use serde::{Deserialize, Serialize};

use crate::mesh::{ChatMessage, LOG_MAX};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Balanced on AC; saver on battery or while Bluetooth audio plays.
    #[default]
    Auto,
    Balanced,
    Saver,
    Off,
}

impl Mode {
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "auto" => Mode::Auto,
            "balanced" => Mode::Balanced,
            "saver" => Mode::Saver,
            "off" => Mode::Off,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub mode: Mode,
    pub persist_history: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { mode: Mode::Auto, persist_history: true }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdentityFile {
    noise_secret: String,
    signing_secret: String,
    nickname: String,
}

pub struct Store {
    data_dir: PathBuf,
    state_dir: PathBuf,
}

impl Store {
    /// `BITCHAT_DATA_DIR` / `BITCHAT_STATE_DIR` (set by the systemd unit, so
    /// they always match its sandbox), else the XDG directories.
    pub fn from_env() -> Result<Store> {
        let explicit = |var: &str| std::env::var_os(var).map(PathBuf::from).filter(|p| p.is_absolute());
        let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME is not set")?;
        let xdg = |var: &str, fallback: &str| {
            std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(fallback))
        };
        Ok(Store::at(
            explicit("BITCHAT_DATA_DIR").unwrap_or_else(|| xdg("XDG_DATA_HOME", ".local/share").join("omarchy-bitchat")),
            explicit("BITCHAT_STATE_DIR").unwrap_or_else(|| xdg("XDG_STATE_HOME", ".local/state").join("omarchy-bitchat")),
        ))
    }

    pub fn at(data_dir: PathBuf, state_dir: PathBuf) -> Store {
        Store { data_dir, state_dir }
    }

    fn ensure_dir(dir: &Path) -> Result<()> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        Ok(())
    }

    /// Load our identity, creating (and saving) a new one on first run.
    pub fn load_identity(&self) -> Result<(Identity, String)> {
        let path = self.data_dir.join("identity.json");
        if let Ok(text) = fs::read_to_string(&path) {
            let file: IdentityFile = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
            let noise = decode_key(&file.noise_secret).context("bad noiseSecret")?;
            let signing = decode_key(&file.signing_secret).context("bad signingSecret")?;
            return Ok((Identity::from_secrets(noise, signing), file.nickname));
        }
        let id = Identity::generate();
        let nickname = format!("anon{}", &id.peer_id().hex()[..4]);
        self.save_identity(&id, &nickname)?;
        Ok((id, nickname))
    }

    pub fn save_identity(&self, id: &Identity, nickname: &str) -> Result<()> {
        Self::ensure_dir(&self.data_dir)?;
        let file = IdentityFile {
            noise_secret: bitchat_proto::peer_id::hex(&id.noise_secret_bytes()),
            signing_secret: bitchat_proto::peer_id::hex(&id.signing_secret_bytes()),
            nickname: nickname.to_owned(),
        };
        write_atomic(&self.data_dir.join("identity.json"), serde_json::to_string_pretty(&file)?.as_bytes())
    }

    pub fn load_settings(&self) -> Settings {
        fs::read_to_string(self.state_dir.join("settings.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<()> {
        Self::ensure_dir(&self.state_dir)?;
        write_atomic(&self.state_dir.join("settings.json"), serde_json::to_string_pretty(s)?.as_bytes())
    }

    fn history_path(&self) -> PathBuf {
        self.state_dir.join("messages.jsonl")
    }

    /// The last [`LOG_MAX`] messages. Compacts the file when it has grown
    /// well past that.
    pub fn load_history(&self) -> Vec<ChatMessage> {
        let Ok(file) = File::open(self.history_path()) else {
            return Vec::new();
        };
        let all: Vec<ChatMessage> = BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect();
        let keep = all[all.len().saturating_sub(LOG_MAX)..].to_vec();
        if all.len() > LOG_MAX * 2 {
            let _ = self.rewrite_history(&keep);
        }
        keep
    }

    pub fn append_history(&self, msg: &ChatMessage) -> Result<()> {
        Self::ensure_dir(&self.state_dir)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.history_path())?;
        writeln!(f, "{}", serde_json::to_string(msg)?)?;
        Ok(())
    }

    pub fn rewrite_history(&self, msgs: &[ChatMessage]) -> Result<()> {
        Self::ensure_dir(&self.state_dir)?;
        let mut body = String::new();
        for m in msgs {
            body.push_str(&serde_json::to_string(m)?);
            body.push('\n');
        }
        write_atomic(&self.history_path(), body.as_bytes())
    }

    pub fn clear_history(&self) -> Result<()> {
        match fs::remove_file(self.history_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

fn decode_key(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// Write via a 0600 temp file and rename, so a crash never leaves half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(name: &str) -> Store {
        let base = std::env::temp_dir().join(format!("bitchatd-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        Store::at(base.join("data"), base.join("state"))
    }

    #[test]
    fn identity_persists_with_private_permissions() {
        let s = temp_store("identity");
        let (a, nick) = s.load_identity().unwrap();
        assert!(nick.starts_with("anon"));
        let (b, nick2) = s.load_identity().unwrap();
        assert_eq!(a.peer_id(), b.peer_id());
        assert_eq!(nick, nick2);
        let mode = fs::metadata(s.data_dir.join("identity.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn settings_round_trip_and_defaults() {
        let s = temp_store("settings");
        assert_eq!(s.load_settings(), Settings::default());
        let custom = Settings { mode: Mode::Saver, persist_history: false };
        s.save_settings(&custom).unwrap();
        assert_eq!(s.load_settings(), custom);
    }

    #[test]
    fn history_append_load_clear() {
        let s = temp_store("history");
        for i in 0..3 {
            s.append_history(&ChatMessage {
                id: format!("{i}"),
                sender_id: "aa".into(),
                nickname: "n".into(),
                text: format!("t{i}"),
                timestamp: i,
                mine: false,
            })
            .unwrap();
        }
        let h = s.load_history();
        assert_eq!(h.len(), 3);
        assert_eq!(h[2].text, "t2");
        s.clear_history().unwrap();
        assert!(s.load_history().is_empty());
    }
}
