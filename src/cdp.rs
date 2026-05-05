//! Minimal CDP (Chrome DevTools Protocol) client for Obscura.
//!
//! Speaks JSON-RPC over WebSocket. Only implements the subset of CDP
//! needed by the Maps scraper: navigate, evaluate, querySelector, click.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::debug;

/// A lightweight CDP page handle.
pub(crate) struct CdpPage {
    tx: mpsc::UnboundedSender<Message>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: AtomicU64,
    session_id: Option<String>,
    _reader_task: tokio::task::JoinHandle<()>,
}

impl CdpPage {
    /// Connect to a CDP WebSocket endpoint, create a target, attach a
    /// session, and return a ready-to-use page handle.
    pub async fn connect(ws_url: &str) -> Result<Self, String> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| format!("WS connect failed: {e}"))?;

        let (mut write, mut read) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

        // Writer task: forward outgoing messages to WebSocket.
        tokio::spawn({
            async move {
                while let Some(msg) = rx.recv().await {
                    if write.send(msg).await.is_err() {
                        break;
                    }
                }
            }
        });

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending.clone();

        // Reader task: route responses to waiting callers.
        let reader_task = tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    _ => continue,
                };
                let Ok(val) = serde_json::from_str::<Value>(&text) else {
                    debug!("non-JSON from CDP: {text}");
                    continue;
                };
                // Match response by id.
                if let Some(id) = val.get("id").and_then(|v| v.as_u64()) {
                    let mut map = pending_clone.lock().await;
                    if let Some(sender) = map.remove(&id) {
                        let _ = sender.send(val);
                    }
                }
                // Events (no id) are silently dropped.
            }
        });

        let mut page = Self {
            tx,
            pending,
            next_id: AtomicU64::new(1),
            session_id: None,
            _reader_task: reader_task,
        };

        // 1) Create a target (page).
        let resp = page
            .send_raw("Target.createTarget", json!({"url": "about:blank"}))
            .await?;
        let target_id = resp
            .pointer("/result/targetId")
            .and_then(|v| v.as_str())
            .ok_or("createTarget: no targetId")?
            .to_string();
        debug!(target_id, "created target");

        // 2) Attach to the target to get a sessionId.
        let resp = page
            .send_raw(
                "Target.attachToTarget",
                json!({"targetId": target_id, "flatten": true}),
            )
            .await?;
        let session_id = resp
            .pointer("/result/sessionId")
            .and_then(|v| v.as_str())
            .ok_or("attachToTarget: no sessionId")?
            .to_string();
        debug!(session_id, "attached to target");

        page.session_id = Some(session_id);

        // 3) Enable Page domain so navigate works.
        let _ = page.send("Page.enable", json!({})).await;

        Ok(page)
    }

    /// Send a CDP command without a session (browser-level).
    async fn send_raw(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });

        let (resp_tx, resp_rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, resp_tx);
        }

        self.tx
            .send(Message::Text(msg.to_string().into()))
            .map_err(|e| format!("send failed: {e}"))?;

        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), resp_rx)
            .await
            .map_err(|_| format!("CDP timeout for {method}"))?
            .map_err(|_| format!("CDP channel closed for {method}"))?;

        if let Some(err) = resp.get("error") {
            return Err(format!("CDP error for {method}: {err}"));
        }

        Ok(resp)
    }

    /// Send a CDP command on the attached session.
    async fn send(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut msg = json!({
            "id": id,
            "method": method,
            "params": params,
        });
        if let Some(sid) = &self.session_id {
            msg["sessionId"] = json!(sid);
        }

        let (resp_tx, resp_rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().await;
            map.insert(id, resp_tx);
        }

        self.tx
            .send(Message::Text(msg.to_string().into()))
            .map_err(|e| format!("send failed: {e}"))?;

        let resp = tokio::time::timeout(std::time::Duration::from_secs(30), resp_rx)
            .await
            .map_err(|_| format!("CDP timeout for {method}"))?
            .map_err(|_| format!("CDP channel closed for {method}"))?;

        if let Some(err) = resp.get("error") {
            return Err(format!("CDP error for {method}: {err}"));
        }

        Ok(resp)
    }

    /// Navigate to a URL and wait for networkidle.
    pub async fn goto(&self, url: &str) -> Result<(), String> {
        self.send("Page.navigate", json!({
            "url": url,
            "waitUntil": "networkidle0"
        })).await?;
        // Extra settle time for JS-heavy pages like Google Maps.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        Ok(())
    }

    /// Evaluate a JS expression and return the result as a serde_json::Value.
    pub async fn evaluate(&self, expression: &str) -> Result<Value, String> {
        let resp = self
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true,
                }),
            )
            .await?;

        let result = resp
            .pointer("/result/result/value")
            .cloned()
            .unwrap_or(Value::Null);
        Ok(result)
    }

    /// Find an element by CSS selector. Returns true if found.
    pub async fn find_element(&self, selector: &str) -> Result<bool, String> {
        let js = format!(
            "document.querySelector({}) !== null",
            serde_json::to_string(selector).unwrap()
        );
        let val = self.evaluate(&js).await?;
        Ok(val.as_bool().unwrap_or(false))
    }

    /// Set cookies via the Network.setCookies CDP method.
    pub async fn set_cookies(&self, cookies: &[(&str, &str, &str)]) -> Result<(), String> {
        let cookie_list: Vec<serde_json::Value> = cookies
            .iter()
            .map(|(name, value, domain)| {
                json!({
                    "name": name,
                    "value": value,
                    "domain": domain,
                    "path": "/",
                })
            })
            .collect();
        self.send("Network.setCookies", json!({"cookies": cookie_list}))
            .await?;
        Ok(())
    }

    /// Click an element matched by CSS selector via JS.
    pub async fn click(&self, selector: &str) -> Result<(), String> {
        let js = format!(
            "(() => {{ const el = document.querySelector({}); if (el) {{ el.click(); return true; }} return false; }})()",
            serde_json::to_string(selector).unwrap()
        );
        self.evaluate(&js).await?;
        Ok(())
    }
}
