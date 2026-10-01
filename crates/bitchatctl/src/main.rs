//! bitchatctl: talk to bitchatd from the terminal.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

const USAGE: &str = "\
usage: bitchatctl <command>

  status            radio state, identity and peer count
  peers             peers on the mesh
  send <text...>    post to #mesh
  tail              print messages as they arrive (Ctrl-C to stop)
  log [n]           the last n messages (default 20)
  nick <name>       change your nickname
  mode <mode>       auto | balanced | saver | off
  forget <peer-id>  forget a peer and the signing key pinned for it
  json <method> [params-json]   raw request, prints the JSON reply";

fn main() {
    if let Err(e) = run() {
        eprintln!("bitchatctl: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        println!("{USAGE}");
        return Ok(());
    };
    let rest = &args[1..];
    let mut client = Client::connect()?;
    match cmd.as_str() {
        "status" => {
            let s = client.call("status", json!({}))?;
            let radio = &s["radio"];
            println!("me       {} ({})", s["me"]["nickname"].as_str().unwrap_or("?"), s["me"]["peerId"].as_str().unwrap_or("?"));
            println!("radio    {}{}", radio["state"].as_str().unwrap_or("?"), detail(radio));
            println!("mode     {} (effective {})", s["settings"]["mode"].as_str().unwrap_or("?"), radio["effective"].as_str().unwrap_or("?"));
            println!("links    {}", radio["links"]);
            println!("peers    {}", s["peers"].as_array().map_or(0, Vec::len));
        }
        "peers" => {
            let s = client.call("status", json!({}))?;
            let peers = s["peers"].as_array().cloned().unwrap_or_default();
            if peers.is_empty() {
                println!("no peers");
            }
            for p in peers {
                println!(
                    "{:<16} {}  {}",
                    safe(p["nickname"].as_str().unwrap_or("?")),
                    p["id"].as_str().unwrap_or("?"),
                    if p["direct"].as_bool() == Some(true) { "direct" } else { "via mesh" }
                );
            }
        }
        "send" => {
            let text = rest.join(" ");
            if text.trim().is_empty() {
                bail!("nothing to send");
            }
            client.call("send", json!({ "text": text }))?;
        }
        "log" => {
            let n: usize = rest.first().map(|s| s.parse()).transpose()?.unwrap_or(20);
            let s = client.call("status", json!({}))?;
            let msgs = s["messages"].as_array().cloned().unwrap_or_default();
            for m in &msgs[msgs.len().saturating_sub(n)..] {
                print_message(m);
            }
        }
        "tail" => {
            client.call("subscribe", json!({}))?;
            loop {
                let v = client.read()?;
                match v["event"].as_str() {
                    Some("message") => print_message(&v["data"]),
                    Some("peers") => println!("-- {} peers", v["data"].as_array().map_or(0, Vec::len)),
                    Some("status") => println!("-- radio {}{}", v["data"]["state"].as_str().unwrap_or("?"), detail(&v["data"])),
                    _ => {}
                }
            }
        }
        "nick" => {
            let nick = rest.join(" ");
            client.call("setNickname", json!({ "nickname": nick }))?;
        }
        "forget" => {
            let id = rest.first().ok_or_else(|| anyhow!("forget needs a peer id (bitchatctl peers)"))?;
            let forgot = client.call("forgetPeer", json!({ "peerId": id }))?;
            println!("{}", if forgot.as_bool() == Some(true) { "forgotten" } else { "not known" });
        }
        "mode" => {
            let mode = rest.first().ok_or_else(|| anyhow!("mode needs auto, balanced, saver or off"))?;
            client.call("setMode", json!({ "mode": mode }))?;
        }
        "json" => {
            let method = rest.first().ok_or_else(|| anyhow!("json needs a method"))?;
            let params: Value = match rest.get(1) {
                Some(p) => serde_json::from_str(p).context("params must be JSON")?,
                None => json!({}),
            };
            println!("{}", serde_json::to_string_pretty(&client.call(method, params)?)?);
        }
        "-h" | "--help" | "help" => println!("{USAGE}"),
        other => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
    Ok(())
}

fn detail(radio: &Value) -> String {
    radio["detail"].as_str().map(|d| format!(" ({d})")).unwrap_or_default()
}

fn print_message(m: &Value) {
    let ts = m["timestamp"].as_u64().unwrap_or(0) / 1000;
    let (h, min) = ((ts / 3600) % 24, (ts / 60) % 60);
    println!(
        "{h:02}:{min:02} UTC <{}> {}",
        safe(m["nickname"].as_str().unwrap_or("?")),
        safe(m["text"].as_str().unwrap_or(""))
    );
}

/// Text from strangers, made safe for a terminal: control characters (escape
/// sequences that could rewrite the screen, the title or the clipboard, and
/// newlines that could fake a line from someone else) are shown escaped.
fn safe(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push_str(&c.escape_unicode().to_string()),
            c => out.push(c),
        }
    }
    out
}


struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    fn connect() -> Result<Client> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
        let path = PathBuf::from(dir).join("bitchat").join("bitchat.sock");
        let stream = UnixStream::connect(&path).with_context(|| {
            format!("can't reach bitchatd at {} (systemctl --user start bitchat)", path.display())
        })?;
        Ok(Client { reader: BufReader::new(stream.try_clone()?), writer: stream, next_id: 1 })
    }

    fn read(&mut self) -> Result<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line)? == 0 {
            bail!("bitchatd closed the connection");
        }
        Ok(serde_json::from_str(&line)?)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(self.writer, "{}", json!({ "id": id, "method": method, "params": params }))?;
        loop {
            let v = self.read()?;
            if v["id"].as_u64() != Some(id) {
                continue; // an event
            }
            if let Some(e) = v["error"].as_str() {
                bail!("{e}");
            }
            return Ok(v["result"].clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::safe;

    #[test]
    fn escapes_terminal_controls() {
        assert_eq!(safe("hi"), "hi");
        assert_eq!(safe("a\nb"), "a\\nb");
        assert_eq!(safe("\u{1b}]52;c;Zm9v\u{7}"), "\\u{1b}]52;c;Zm9v\\u{7}");
        assert_eq!(safe("\u{9b}2J"), "\\u{9b}2J");
    }
}
