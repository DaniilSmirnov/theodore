//! JSON-RPC 2.0 over a child's stdio with LSP's `Content-Length` framing.
//!
//! ```text
//! request() ─▶ pending[id] ─▶ out channel ─▶ writer task ─▶ child stdin
//!                  ▲
//! child stdout ─▶ reader task ─┬─ response      ─▶ pending[id] ─▶ caller
//!                              ├─ request       ─▶ Handler::request ─▶ reply
//!                              └─ notification  ─▶ Handler::notification
//! stdout EOF ─▶ every pending request fails with `Closed`
//! ```
//!
//! The handler runs on the reader task and must not block: server→client
//! requests are answered from in-memory state, in arrival order.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

/// JSON-RPC "method not found".
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;

/// Why a request got no result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RpcError {
    /// No response within the request's timeout.
    Timeout { method: String, after: Duration },
    /// The server's output closed (exit or crash) before it answered.
    Closed,
    /// The server answered with an error object.
    Server { code: i64, message: String },
    /// The response did not parse as the expected type.
    Decode(String),
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::Timeout { method, after } => {
                write!(f, "{method} timed out after {} ms", after.as_millis())
            }
            RpcError::Closed => f.write_str("server exited"),
            RpcError::Server { code, message } => write!(f, "error {code}: {message}"),
            RpcError::Decode(e) => write!(f, "bad response: {e}"),
        }
    }
}

/// What the reader task does with traffic the server starts.
pub(crate) trait Handler: Send + Sync + 'static {
    /// Answer a server→client request: `Ok(result)` or `Err((code, message))`.
    fn request(&self, method: &str, params: Value) -> Result<Value, (i64, String)>;
    fn notification(&self, method: &str, params: Value);
    /// The server's output closed. Called once.
    fn closed(&self);
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// The client end of one connection.
pub(crate) struct Rpc {
    out: mpsc::UnboundedSender<Value>,
    pending: Pending,
    next_id: AtomicI64,
    closed: Arc<AtomicBool>,
}

impl Rpc {
    /// Start the reader and writer tasks over `reader` / `writer`.
    pub(crate) fn start<R, W>(reader: R, writer: W, handler: Arc<dyn Handler>) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (out, out_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let closed = Arc::new(AtomicBool::new(false));
        tokio::spawn(write_loop(writer, out_rx));
        tokio::spawn(read_loop(
            BufReader::new(reader),
            out.clone(),
            pending.clone(),
            closed.clone(),
            handler,
        ));
        Self {
            out,
            pending,
            next_id: AtomicI64::new(1),
            closed,
        }
    }

    /// The server's output has closed.
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Send a request and wait up to `timeout` for its result. A timed-out
    /// request is cancelled with `$/cancelRequest`.
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcError> {
        if self.is_closed() {
            return Err(RpcError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.pending).insert(id, tx);
        // The reader may have drained `pending` between the check above and
        // the insert; check again so the caller never waits for nothing.
        if self.is_closed() {
            lock(&self.pending).remove(&id);
            return Err(RpcError::Closed);
        }
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if self.out.send(msg).is_err() {
            lock(&self.pending).remove(&id);
            return Err(RpcError::Closed);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Closed),
            Err(_) => {
                lock(&self.pending).remove(&id);
                self.notify("$/cancelRequest", json!({ "id": id }));
                Err(RpcError::Timeout {
                    method: method.to_string(),
                    after: timeout,
                })
            }
        }
    }

    /// Send a notification. Dropped silently once the connection is gone.
    pub(crate) fn notify(&self, method: &str, params: Value) {
        let _ = self
            .out
            .send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

async fn write_loop<W: AsyncWrite + Unpin>(mut writer: W, mut rx: mpsc::UnboundedReceiver<Value>) {
    while let Some(msg) = rx.recv().await {
        let body = msg.to_string();
        let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
        if writer.write_all(frame.as_bytes()).await.is_err() || writer.flush().await.is_err() {
            break;
        }
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    mut reader: BufReader<R>,
    out: mpsc::UnboundedSender<Value>,
    pending: Pending,
    closed: Arc<AtomicBool>,
    handler: Arc<dyn Handler>,
) {
    loop {
        let msg = match read_message(&mut reader).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(error = %e, "lsp: unreadable message, closing");
                break;
            }
        };
        dispatch(msg, &out, &pending, handler.as_ref());
    }
    closed.store(true, Ordering::SeqCst);
    for (_, tx) in lock(&pending).drain() {
        let _ = tx.send(Err(RpcError::Closed));
    }
    handler.closed();
}

fn dispatch(
    mut msg: Value,
    out: &mpsc::UnboundedSender<Value>,
    pending: &Pending,
    handler: &dyn Handler,
) {
    let id = msg.get_mut("id").map(Value::take);
    let method = msg
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_string);
    let params = msg
        .get_mut("params")
        .map(Value::take)
        .unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), Some(method)) => {
            let reply = match handler.request(&method, params) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": {"code": code, "message": message},
                }),
            };
            let _ = out.send(reply);
        }
        (None, Some(method)) => handler.notification(&method, params),
        (Some(id), None) => {
            let Some(id) = id.as_i64() else { return };
            let Some(tx) = lock(pending).remove(&id) else {
                return;
            };
            let result = match msg.get_mut("error") {
                Some(err) => Err(RpcError::Server {
                    code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                }),
                None => Ok(msg
                    .get_mut("result")
                    .map(Value::take)
                    .unwrap_or(Value::Null)),
            };
            let _ = tx.send(result);
        }
        (None, None) => {}
    }
}

/// One framed message; `None` at a clean EOF.
pub(crate) async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> std::io::Result<Option<Value>> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        let header = line.trim_end();
        if header.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            len = value.trim().parse().ok();
        }
    }
    let mut body = vec![0; len.unwrap_or(0)];
    reader.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reads_framed_messages_back_to_back() {
        let a = r#"{"jsonrpc":"2.0","method":"a"}"#;
        let b = r#"{"jsonrpc":"2.0","id":1,"result":"é"}"#;
        let raw = format!(
            "Content-Length: {}\r\n\r\n{a}Content-Type: x\r\nContent-Length: {}\r\n\r\n{b}",
            a.len(),
            b.len()
        );
        let mut reader = BufReader::new(raw.as_bytes());
        let first = read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(first["method"], "a");
        let second = read_message(&mut reader).await.unwrap().unwrap();
        assert_eq!(second["result"], "é");
        assert!(read_message(&mut reader).await.unwrap().is_none());
    }
}
