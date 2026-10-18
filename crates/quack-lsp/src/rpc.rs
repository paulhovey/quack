//! JSON-RPC over stdio with LSP's `Content-Length` framing.

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Messages larger than this are a broken server, not a big workspace.
const MAX_MESSAGE: usize = 64 * 1024 * 1024;

/// Read one message. `Ok(None)` at end of stream.
pub async fn read(r: &mut (impl AsyncBufRead + Unpin)) -> std::io::Result<Option<Value>> {
    let mut length = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            if length.is_some() {
                break;
            }
            // Stray blank line between messages.
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.unwrap_or_default();
    if length > MAX_MESSAGE {
        return Err(std::io::Error::other(format!(
            "message of {length} bytes is too large"
        )));
    }
    let mut body = vec![0u8; length];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(std::io::Error::other)
}

pub async fn write(w: &mut (impl AsyncWrite + Unpin), msg: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(&body);
    w.write_all(&out).await?;
    w.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn round_trip() {
        let mut buf = Vec::new();
        write(&mut buf, &json!({"jsonrpc": "2.0", "id": 1, "result": "é"}))
            .await
            .expect("write");
        write(&mut buf, &json!({"jsonrpc": "2.0", "method": "x"}))
            .await
            .expect("write");
        let mut r = tokio::io::BufReader::new(&buf[..]);
        let a = read(&mut r).await.expect("read").expect("message");
        assert_eq!(a["result"], "é");
        let b = read(&mut r).await.expect("read").expect("message");
        assert_eq!(b["method"], "x");
        assert!(read(&mut r).await.expect("eof").is_none());
    }

    #[tokio::test]
    async fn extra_headers_are_ignored() {
        let raw = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: 2\r\n\r\n{}";
        let mut r = tokio::io::BufReader::new(&raw[..]);
        assert_eq!(read(&mut r).await.expect("read"), Some(json!({})));
    }
}
