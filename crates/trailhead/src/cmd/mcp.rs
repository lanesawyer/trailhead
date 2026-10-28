use anyhow::Result;
use serde_json::{Value, json};

use crate::client::Client;

/// Newline-delimited JSON-RPC on stdio, one HTTP POST per message. Only JSON-RPC
/// goes to stdout; everything else goes to stderr.
pub(crate) async fn bridge(c: &Client) -> Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    let mut protocol: Option<String> = None;
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("trailhead mcp: ignoring unparseable message: {e}");
                continue;
            }
        };
        let replies = match c.mcp_post(&line, protocol.as_deref()).await {
            Ok(r) => r,
            Err(e) if msg.get("id").is_some() => {
                let err = json!({ "jsonrpc": "2.0", "id": msg["id"], "error": { "code": -32603, "message": format!("{e}") } });
                vec![err.to_string()]
            }
            Err(e) => {
                eprintln!("trailhead mcp: {e}");
                continue;
            }
        };
        for reply in replies {
            if msg["method"] == "initialize"
                && let Some(v) = serde_json::from_str::<Value>(&reply)
                    .ok()
                    .and_then(|r| r["result"]["protocolVersion"].as_str().map(String::from))
            {
                protocol = Some(v);
            }
            stdout.write_all(reply.as_bytes()).await?;
            stdout.write_all(b"\n").await?;
        }
        stdout.flush().await?;
    }
    Ok(())
}
