# Omarchy Bitchat

[bitchat](https://github.com/permissionlesstech/bitchat) mesh chat in the [Omarchy](https://omarchy.org) bar. Your laptop joins the same Bluetooth mesh as the bitchat apps on Android and iOS: it finds phones nearby, relays their traffic, and lets you talk in `#mesh` without internet, accounts or servers.

- **On the bar:** a chat glyph with the number of peers in range. It lights up when there's something unread. Left click opens the panel, right click opens it ready to type, middle click turns the mesh on or off.
- **In the panel:** `#mesh`, the public channel everyone in Bluetooth range shares. Messages are signed and checked. Mentions of `@yourname` are highlighted. The input line takes `/nick`, `/who`, `/clear` and `/help`, and Up recalls what you sent.
- **A real mesh node.** It relays other people's packets, including private messages and files it can't read, so the laptop extends the mesh. When a phone shows up, it backfills the messages each side missed (gossip sync).
- **Easy on the radio.** Scanning runs in bursts (8 s of every 10 on AC power; 2 s of every 30 on battery or while Bluetooth headphones are playing), so it doesn't stutter your audio. Phones can always connect to the laptop. Off turns everything off.
- **Notifications** when someone mentions you (or for everything, or never), but not while the panel is open.

Private messages, location (geohash) channels over Nostr and favorites are next; see [Roadmap](#roadmap).

## Keys

| Key | Does |
| --- | --- |
| `Enter` | Send |
| `Up` / `Down` | Recall what you sent |
| `PgUp` / `PgDn` | Scroll the chat |
| `Esc` | Clear the line, then close |
| `Tab` | Next bar panel |

| Command | Does |
| --- | --- |
| `/nick <name>` | Change your nickname (up to 15 characters) |
| `/who` | Show the peers in range |
| `/clear` | Clear the chat history |
| `//text` | Send a line that starts with `/` |

## Requirements

- Omarchy 4 or newer (the Quickshell `omarchy-shell`)
- A Bluetooth adapter that supports LE peripheral mode, which most from the last ten years do. `bluetoothctl show` should list `Roles: peripheral`.
- BlueZ with its default settings (Omarchy ships 5.87). No configuration changes and no root.
- Rust to build the daemon: the `rustup` package, then `rustup default stable`

## Install

```bash
omarchy plugin add https://github.com/derekross/omarchy-bitchat
~/.config/omarchy/plugins/derekross.bitchat/dist/install.sh
omarchy plugin enable derekross.bitchat --section right
```

`install.sh` builds `bitchatd` (the daemon) and `bitchatctl` (a command-line client) and puts them in `~/.local/bin`. It installs and starts the `bitchat.service` user unit and links the plugin in place. It never uses sudo. It won't overwrite a file it didn't write unless you pass `--replace-existing`, and in that case it keeps a backup.

From a checkout instead: `git clone … && cd omarchy-bitchat && ./dist/install.sh`, which also links the checkout into `~/.config/omarchy/plugins`.

## Settings

| Setting | Default | |
| --- | --- | --- |
| `notify` | `mentions` | `mentions`, `all` or `off` |
| `showPeerCount` | `true` | Peer count next to the glyph |
| `persistHistory` | `true` | Keep the last 500 messages across restarts. Off deletes them. |
| `clock24h` | `true` | 24-hour message times |

The radio mode (Auto, Active, Saver, Off) and your nickname live in the panel's settings and are kept by the daemon.

## From the terminal

```bash
bitchatctl status          # radio, identity, peers
bitchatctl peers
bitchatctl send hello mesh
bitchatctl tail            # follow the chat
bitchatctl mode saver      # auto | balanced | saver | off
omarchy-shell derekross.bitchat toggle   # for a keybinding
```

## Update

```bash
omarchy plugin update derekross.bitchat
~/.config/omarchy/plugins/derekross.bitchat/dist/install.sh
```

## Remove

```bash
~/.config/omarchy/plugins/derekross.bitchat/dist/uninstall.sh      # --purge also deletes your identity and history
omarchy plugin remove derekross.bitchat
```

## Privacy and security

- **Public is public.** `#mesh` messages are signed but not encrypted, like on the phone apps. Anyone in range running bitchat can read them, and so can anyone listening to Bluetooth.
- **Your identity** is a pair of keys made on first run: Curve25519 for Noise and Ed25519 for signing. They're in `~/.local/share/omarchy-bitchat/identity.json` (mode 0600) and never leave the machine. Your peer ID is derived from the Noise key. Delete the file, or `uninstall.sh --purge`, to start over as someone new.
- **Your laptop advertises** a bitchat service while the mesh is on. Nearby devices can tell a bitchat node is there, though not who you are beyond your nickname. Off stops all advertising.
- **Only what verifies.** Messages and announces whose signature doesn't check out are dropped and never relayed. A peer can't take over a known peer ID with a different key.
- **The control socket** (`$XDG_RUNTIME_DIR/bitchat.sock`) only accepts connections from your own user.
- **The daemon runs sandboxed** (see `dist/bitchat.service`): it can write only its own data and state folders, can open only Unix sockets (Bluetooth goes through BlueZ over D-Bus), and never dumps core.

## How it works

```
phones ⇄ BLE ⇄ BlueZ ⇄ D-Bus ⇄ bitchatd ⇄ bitchat.sock (JSON lines) ⇄ Service.qml ⇄ bar + panel
```

- `crates/bitchat-proto` implements the bitchat wire protocol, byte-compatible with [bitchat-android](https://github.com/permissionlesstech/bitchat-android) and [bitchat for iOS](https://github.com/permissionlesstech/bitchat): packets v1 and v2, padding, raw-deflate compression, Ed25519 signing, fragmentation, announces, dedup, relay policy and GCS gossip sync. It's pure and unit-tested.
- `crates/bitchatd` uses [bluer](https://github.com/bluez/bluer) to be both a GATT peripheral (advertises service `F47B5E2D-…-4B5C`, hosts the characteristic) and a GATT central (scans, connects, writes, subscribes), like the phone apps. A pure mesh engine (`mesh.rs`) makes every protocol decision. It survives BlueZ restarts and the adapter being switched off and on, and it sends a LEAVE when it stops.
- The QML side follows the other Omarchy plugins: one `Service.qml` connection shared by every monitor's bar widget, and a `KeyboardPanel` popup styled from the theme.

Logs: `journalctl --user -u bitchat.service`. For more detail, set `BITCHAT_LOG=bitchatd=debug` in the unit.

## Roadmap

1. **Private messages:** Noise XX sessions with delivery and read receipts, and a DM view in the panel.
2. **Location channels:** geohash channels over Nostr (kind 20000), ported from [BitchatX](https://github.com/derekross/bitchatx).
3. **Nostr fallback for DMs** to mutual favorites (NIP-17).

## Development

```bash
cargo test --workspace          # protocol, mesh engine, store, power
node --test tests/              # Model.js
omarchy plugin validate .
BITCHAT_LOG=bitchatd=debug cargo run -p bitchatd   # stop bitchat.service first
```

## License

MIT
