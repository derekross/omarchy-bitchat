//! On-disk state: identity (keys + nickname), settings, and message history.
//!
//! - `$XDG_DATA_HOME/omarchy-bitchat/identity.json` (0600): who we are.
//! - `$XDG_STATE_HOME/omarchy-bitchat/settings.json`: radio mode, history.
//! - `$XDG_STATE_HOME/omarchy-bitchat/messages.jsonl`: recent public chat.

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
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

    /// Create the folder if needed. It must be a real folder we own, not a
    /// symlink; it's then made private (0700).
    fn ensure_dir(dir: &Path) -> Result<()> {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let meta = fs::symlink_metadata(dir)?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
            anyhow::bail!("{} must be a folder you own, not a link", dir.display());
        }
        if meta.permissions().mode() & 0o777 != 0o700 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    /// Load our identity, creating (and saving) a new one on first run.
    pub fn load_identity(&self) -> Result<(Identity, String)> {
        let path = self.data_dir.join("identity.json");
        match read_small(&path, 64 * 1024) {
            Ok(text) => {
                let file: IdentityFile =
                    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                let noise = decode_key(&file.noise_secret).context("bad noiseSecret")?;
                let signing = decode_key(&file.signing_secret).context("bad signingSecret")?;
                return Ok((Identity::from_secrets(noise, signing), file.nickname));
            }
            // Only a missing file means "first run". Any other error (a
            // permission problem, a disk error) must not quietly replace
            // the user's identity with a new one.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(anyhow::Error::from(e).context(format!("reading {}", path.display()))),
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
        read_small(&self.state_dir.join("settings.json"), 64 * 1024)
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

    /// The last [`LOG_MAX`] messages, read from at most the last
    /// [`HISTORY_MAX_BYTES`] of the file. Compacts the file when it has
    /// grown well past that.
    pub fn load_history(&self) -> Vec<ChatMessage> {
        let Ok(mut file) = open_nofollow(&self.history_path()) else {
            return Vec::new();
        };
        let len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let skip = len.saturating_sub(HISTORY_MAX_BYTES);
        if skip > 0 && file.seek(SeekFrom::Start(skip)).is_err() {
            return Vec::new();
        }
        let mut lines = BufReader::new(file).lines().map_while(Result::ok);
        if skip > 0 {
            lines.next(); // probably a partial line
        }
        let all: Vec<ChatMessage> = lines.filter_map(|l| serde_json::from_str(&l).ok()).collect();
        let keep = all[all.len().saturating_sub(LOG_MAX)..].to_vec();
        if all.len() > LOG_MAX * 2 || skip > 0 {
            let _ = self.rewrite_history(&keep);
        }
        keep
    }

    /// Append one message. Returns true when the file has grown enough that
    /// the caller should compact it with [`Store::rewrite_history`].
    pub fn append_history(&self, msg: &ChatMessage) -> Result<bool> {
        Self::ensure_dir(&self.state_dir)?;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.history_path())?;
        writeln!(f, "{}", serde_json::to_string(msg)?)?;
        Ok(f.metadata()?.len() > HISTORY_MAX_BYTES)
    }

    /// Peer IDs and the signing key each first used (see `Mesh::pins`).
    pub fn load_pins(&self) -> Vec<(bitchat_proto::PeerId, [u8; 32])> {
        let Ok(text) = read_small(&self.state_dir.join("peers.json"), 2 * 1024 * 1024) else {
            return Vec::new();
        };
        let pairs: Vec<(String, String)> = serde_json::from_str(&text).unwrap_or_default();
        pairs
            .iter()
            .filter_map(|(id, key)| Some((bitchat_proto::PeerId::from_hex(id)?, decode_key(key)?)))
            .collect()
    }

    pub fn save_pins(&self, pins: &[(bitchat_proto::PeerId, [u8; 32])]) -> Result<()> {
        Self::ensure_dir(&self.state_dir)?;
        let pairs: Vec<(String, String)> =
            pins.iter().map(|(id, key)| (id.hex(), bitchat_proto::peer_id::hex(key))).collect();
        write_atomic(&self.state_dir.join("peers.json"), serde_json::to_string(&pairs)?.as_bytes())
    }

    /// Rewrite the file with the newest messages that fit in half of
    /// [`HISTORY_MAX_BYTES`] (so it doesn't need compacting again at once).
    pub fn rewrite_history(&self, msgs: &[ChatMessage]) -> Result<()> {
        Self::ensure_dir(&self.state_dir)?;
        let budget = (HISTORY_MAX_BYTES / 2) as usize;
        let mut lines: Vec<String> = Vec::new();
        let mut size = 0;
        for m in msgs.iter().rev() {
            let line = serde_json::to_string(m)?;
            if size + line.len() + 1 > budget {
                break;
            }
            size += line.len() + 1;
            lines.push(line);
        }
        lines.reverse();
        let mut body = lines.join("\n");
        if !body.is_empty() {
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

/// Most of the history file we ever read back.
pub const HISTORY_MAX_BYTES: u64 = 2 * 1024 * 1024;

fn open_nofollow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path)
}

/// Read a small file, refusing symlinks and anything over `max` bytes.
fn read_small(path: &Path, max: u64) -> std::io::Result<String> {
    let f = open_nofollow(path)?;
    let mut text = String::new();
    f.take(max + 1).read_to_string(&mut text)?;
    if text.len() as u64 > max {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "file too large"));
    }
    Ok(text)
}

/// Write via a fresh 0600 temp file with an unpredictable name (created
/// exclusively, never through a link) and rename it over `path`, so a crash
/// never leaves half a file and nothing at a guessable name is followed.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    let name = path.file_name().context("path has no file name")?.to_string_lossy();
    let mut attempt = 0;
    let (tmp, mut f) = loop {
        let tmp = dir.join(format!(".{name}.{:016x}", rand::random::<u64>()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
        {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => attempt += 1,
            Err(e) => return Err(anyhow::Error::from(e).context(format!("writing next to {}", path.display()))),
        }
    };
    let written = f.write_all(bytes).and_then(|_| f.sync_all());
    drop(f);
    if let Err(e) = written.and_then(|_| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow::Error::from(e).context(format!("replacing {}", path.display())));
    }
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in a fresh private temp folder, removed when dropped.
    fn temp_store() -> (tempfile::TempDir, Store) {
        let base = tempfile::tempdir().unwrap();
        let store = Store::at(base.path().join("data"), base.path().join("state"));
        (base, store)
    }

    #[test]
    fn identity_persists_with_private_permissions() {
        let (_tmp, s) = temp_store();
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
        let (_tmp, s) = temp_store();
        assert_eq!(s.load_settings(), Settings::default());
        let custom = Settings { mode: Mode::Saver, persist_history: false };
        s.save_settings(&custom).unwrap();
        assert_eq!(s.load_settings(), custom);
    }

    #[test]
    fn history_compacts_to_a_byte_budget() {
        let (_tmp, s) = temp_store();
        let big = |i: usize| ChatMessage {
            id: format!("{i}"),
            sender_id: "aa".into(),
            nickname: "n".into(),
            text: "\"".repeat(4000), // escapes to 8000 bytes of JSON
            timestamp: i as u64,
            mine: false,
        };
        let msgs: Vec<_> = (0..500).map(big).collect();
        s.rewrite_history(&msgs).unwrap();
        let len = fs::metadata(s.history_path()).unwrap().len();
        assert!(len <= HISTORY_MAX_BYTES / 2, "{len}");
        // Appending stays under the threshold for a while: no rewrite storm.
        assert!(!s.append_history(&big(501)).unwrap());
        let back = s.load_history();
        assert_eq!(back.last().unwrap().id, "501");
    }

    #[test]
    fn history_append_load_clear() {
        let (_tmp, s) = temp_store();
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
