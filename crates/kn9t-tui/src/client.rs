//! HTTP client for server communication.
//!
//! R-TUI-010: Uses ureq, no kn9t-* deps.

use std::io::{BufRead, BufReader};
use std::sync::mpsc::Sender;
use std::thread;

use crate::event::Event;
use crate::wire::*;

#[derive(Debug)]
pub enum ClientError {
    Http(String),
    Json(String),
    SessionBusy,
    NotFound,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Http(s) => write!(f, "HTTP error: {}", s),
            ClientError::Json(s) => write!(f, "JSON error: {}", s),
            ClientError::SessionBusy => write!(f, "Session busy (another client holds lease)"),
            ClientError::NotFound => write!(f, "Not found"),
        }
    }
}

pub struct Client {
    base_url: String,
    token: Option<String>,
}

impl Client {
    pub fn new(base_url: &str, token: Option<&str>) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.map(|s| s.to_string()),
        }
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let url = format!("{}{}", self.base_url, path);
        let req = match method {
            "GET" => ureq::get(&url),
            "POST" => ureq::post(&url),
            "PUT" => ureq::put(&url),
            "DELETE" => ureq::delete(&url),
            _ => ureq::get(&url),
        };
        if let Some(t) = &self.token {
            req.set("Authorization", &format!("Bearer {}", t))
        } else {
            req
        }
    }

    /// List all sessions.
    pub fn list_sessions(&self) -> Result<Vec<SessionInfo>, ClientError> {
        let resp = self.request("GET", "/session")
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: SessionList = resp.into_json()
            .map_err(|e| ClientError::Json(e.to_string()))?;
        Ok(body.sessions)
    }

    /// Get session detail.
    pub fn get_session(&self, id: &str) -> Result<SessionDetail, ClientError> {
        let resp = self.request("GET", &format!("/session/{}", id))
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        resp.into_json().map_err(|e| ClientError::Json(e.to_string()))
    }

    /// Create a new session.
    pub fn create_session(&self, cwd: &str, model: Option<&WireModelRef>) -> Result<String, ClientError> {
        let req = CreateSessionReq {
            cwd: Some(cwd.to_string()),
            model: model.cloned(),
            name: None,
        };
        let resp = self.request("POST", "/session")
            .send_json(&req)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: serde_json::Value = resp.into_json()
            .map_err(|e| ClientError::Json(e.to_string()))?;
        body["id"].as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| ClientError::Json("missing id".into()))
    }

    /// Delete a session.
    pub fn delete_session(&self, session_id: &str) -> Result<(), ClientError> {
        let path = format!("/session/{}", session_id);
        let resp = self.request("DELETE", &path)
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        if resp.status() >= 400 {
            return Err(ClientError::Http(format!("delete failed: {}", resp.status())));
        }
        Ok(())
    }

    /// Acquire write lease.
    pub fn acquire_lease(&self, session_id: &str, takeover: bool) -> Result<String, ClientError> {
        let path = if takeover {
            format!("/session/{}/lease?takeover=1", session_id)
        } else {
            format!("/session/{}/lease", session_id)
        };
        let resp = self.request("POST", &path)
            .call();
        
        match resp {
            Ok(r) => {
                let body: serde_json::Value = r.into_json()
                    .map_err(|e| ClientError::Json(e.to_string()))?;
                // Server returns { "lease": holder, "session": id }
                body["lease"].as_str()
                    .map(|s| s.to_string())
                    .ok_or_else(|| ClientError::Json("missing lease".into()))
            }
            Err(ureq::Error::Status(409, _)) => Err(ClientError::SessionBusy),
            Err(e) => Err(ClientError::Http(e.to_string())),
        }
    }

    /// Release lease.
    pub fn release_lease(&self, session_id: &str, holder: &str) -> Result<(), ClientError> {
        self.request("DELETE", &format!("/session/{}/lease", session_id))
            .set("X-Lease", holder)
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }

    /// Send prompt.
    pub fn prompt(&self, session_id: &str, holder: &str, text: &str, images: Vec<String>) -> Result<(), ClientError> {
        let req = PromptReq {
            text: Some(text.to_string()),
            blobs: None,
            images: if images.is_empty() { None } else { Some(images) },
        };
        self.request("POST", &format!("/session/{}/prompt", session_id))
            .set("X-Lease", holder)
            .send_json(&req)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }

    /// Abort current turn.
    pub fn abort(&self, session_id: &str, holder: &str) -> Result<(), ClientError> {
        self.request("POST", &format!("/session/{}/abort", session_id))
            .set("X-Lease", holder)
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }

    /// Steer: inject a user message while a turn is running.
    /// The message is appended to the session and folded into the next LLM call.
    pub fn steer(&self, session_id: &str, holder: &str, text: &str) -> Result<u64, ClientError> {
        let req = SteerReq { text: text.to_string() };
        let resp = self.request("POST", &format!("/session/{}/steer", session_id))
            .set("X-Lease", holder)
            .send_json(&req)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: serde_json::Value = resp.into_json()
            .map_err(|e| ClientError::Json(e.to_string()))?;
        Ok(body["seq"].as_u64().unwrap_or(0))
    }

    /// Respond to approval request.
    pub fn approve(&self, session_id: &str, holder: &str, id: u64, decision: &str) -> Result<(), ClientError> {
        // Map legacy "always" decision to scope=always for schema correctness (Phase 2).
        let (decision_str, scope) = match decision {
            "always" => ("allow".to_string(), Some("always".to_string())),
            other => (other.to_string(), None),
        };
        let req = ApprovalResp { id, decision: decision_str, scope };
        self.request("POST", "/approve")
            .set("X-Lease", holder)
            .set("X-Lease-Session", session_id)
            .send_json(&req)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }

    /// Scope-aware approve (Phase 1.5): decision allow|deny + scope once|session|always.
    pub fn approve_scoped(&self, session_id: &str, holder: &str, id: u64, decision: &str, scope: &str) -> Result<(), ClientError> {
        let req = ApprovalResp { id, decision: decision.to_string(), scope: Some(scope.to_string()) };
        self.request("POST", "/approve")
            .set("X-Lease", holder)
            .set("X-Lease-Session", session_id)
            .send_json(&req)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }

    /// List available models.
    pub fn list_models(&self) -> Result<Vec<ModelInfo>, ClientError> {
        let resp = self.request("GET", "/models")
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: ModelsList = resp.into_json()
            .map_err(|e| ClientError::Json(e.to_string()))?;
        Ok(body.models)
    }

    /// Upload blob.
    pub fn upload_blob(&self, data: &[u8], mime: &str) -> Result<String, ClientError> {
        let resp = self.request("POST", "/blob")
            .set("Content-Type", mime)
            .send_bytes(data)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: serde_json::Value = resp.into_json()
            .map_err(|e| ClientError::Json(e.to_string()))?;
        body["hash"].as_str()
            .map(|s| format!("sha256:{}", s))
            .ok_or_else(|| ClientError::Json("missing hash".into()))
    }
    
    /// Get a preference value.
    pub fn get_pref(&self, key: &str) -> Option<String> {
        let resp = self.request("GET", &format!("/pref/{}", key))
            .call()
            .ok()?;
        let body: serde_json::Value = resp.into_json().ok()?;
        body["value"].as_str().map(|s| s.to_string())
    }
    
    /// Set a preference value.
    pub fn set_pref(&self, key: &str, value: &str) -> Result<(), ClientError> {
        self.request("PUT", &format!("/pref/{}", key))
            .send_string(value)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }
    
    /// Change model for a session. Requires lease.
    pub fn set_model(&self, session_id: &str, lease: &str, provider: &str, model_id: &str) -> Result<(), ClientError> {
        crate::log!("[client.set_model] session={} provider={} model_id={}", session_id, provider, model_id);
        let body = serde_json::json!({
            "provider": provider,
            "id": model_id
        });
        let url = format!("/session/{}/model", session_id);
        crate::log!("[client.set_model] POST {} with lease={}", url, lease);
        let resp = self.request("POST", &url)
            .set("X-Lease", lease)
            .send_json(&body)
            .map_err(|e| {
                crate::log!("[client.set_model] HTTP error: {}", e);
                ClientError::Http(e.to_string())
            })?;
        crate::log!("[client.set_model] response status={}", resp.status());
        Ok(())
    }

    /// List registered tools (GET /tools?session= — F9, Phase 4).
    /// Returns full tool info from discovered + pinned plugins, with per-session disabled state.
    pub fn get_tools(&self, session_id: Option<&str>) -> Result<Vec<crate::app::ToolEntry>, ClientError> {
        let path = match session_id {
            Some(sid) => format!("/tools?session={}", sid),
            None => "/tools".to_string(),
        };
        let resp = self.request("GET", &path)
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: serde_json::Value = resp.into_json().map_err(|e| ClientError::Json(e.to_string()))?;
        let tools = body.get("tools").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let entries: Vec<crate::app::ToolEntry> = tools
            .iter()
            .map(|v| crate::app::ToolEntry {
                name: v.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                description: v.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
                plugin: v.get("plugin").and_then(|p| p.as_str()).map(|s| s.to_string()),
                enabled: !v.get("disabled").and_then(|d| d.as_bool()).unwrap_or(false),
            })
            .collect();
        Ok(entries)
    }

    /// Set tools disabled for a session (POST /session/{id}/tools — action endpoint).
    /// Returns the list of tools that were re-enabled.
    pub fn set_tools(&self, session_id: &str, lease: &str, disabled: &[String]) -> Result<Vec<String>, ClientError> {
        let body = serde_json::json!({ "disabled": disabled });
        let resp = self.request("POST", &format!("/session/{}/tools", session_id))
            .set("X-Lease", lease)
            .send_json(&body)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: serde_json::Value = resp.into_json().map_err(|e| ClientError::Json(e.to_string()))?;
        let reenabled: Vec<String> = body
            .get("reenabled")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        Ok(reenabled)
    }

    /// Rename a session (POST /session/{id}/rename — Phase 4 action endpoint, no PATCH).
    pub fn rename_session(&self, session_id: &str, new_name: &str) -> Result<(), ClientError> {
        let body = serde_json::json!({ "name": new_name });
        self.request("POST", &format!("/session/{}/rename", session_id))
            .send_json(&body)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }

    /// Trigger manual compaction (POST /session/{id}/compact — Phase 4, lease required, engine at exec.rs:139).
    pub fn compact_session(&self, session_id: &str, lease: &str) -> Result<u64, ClientError> {
        let resp = self.request("POST", &format!("/session/{}/compact", session_id))
            .set("X-Lease", lease)
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        let body: serde_json::Value = resp.into_json().map_err(|e| ClientError::Json(e.to_string()))?;
        Ok(body.get("seq").and_then(|v| v.as_u64()).unwrap_or(0))
    }

    /// Export a session transcript (GET /session/{id}/export — Phase 4, replaces placeholder).
    pub fn export_session(&self, session_id: &str) -> Result<serde_json::Value, ClientError> {
        let resp = self.request("GET", &format!("/session/{}/export", session_id))
            .call()
            .map_err(|e| ClientError::Http(e.to_string()))?;
        resp.into_json().map_err(|e| ClientError::Json(e.to_string()))
    }

    /// 96E-28: respond to a pending generic interaction — opaque payload forwarded verbatim.
    pub fn ui_respond(&self, id: u64, payload: serde_json::Value) -> Result<(), ClientError> {
        let req = UiRespondReq { id, payload };
        self.request("POST", "/ui-respond")
            .send_json(&req)
            .map_err(|e| ClientError::Http(e.to_string()))?;
        Ok(())
    }
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Handle to stop an SSE thread.
#[derive(Clone)]
pub struct SseHandle {
    stop: Arc<AtomicBool>,
    session_id: String,
}

impl SseHandle {
    /// Signal the SSE thread to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
    
    /// Get the session ID this handle is for.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Spawn SSE reader thread. Returns a handle to stop it.
///
/// `lease` is the holder token this client acquired for the session (if any). It is
/// passed to the server as `?lease=` so the stream *owns* the lease and keeps it
/// warm while connected — otherwise a client that reads for >5 min without writing
/// idle-loses its lease and its next prompt 409s (server DESIGN §12.6).
pub fn spawn_sse_thread(
    base_url: String,
    token: Option<String>,
    session_id: String,
    from_seq: u64,
    lease: Option<String>,
    tx: Sender<Event>,
) -> SseHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    let session_id_clone = session_id.clone();
    
    crate::log!("SSE: spawning thread for session {}", session_id);
    
    thread::spawn(move || {
        let mut url = format!("{}/session/{}/events?from={}", base_url, session_id_clone, from_seq);
        if let Some(l) = &lease {
            url.push_str(&format!("&lease={}", l));
        }
        crate::log!("SSE: connecting to {}", url);
        
        let mut req = ureq::get(&url);
        if let Some(t) = &token {
            req = req.set("Authorization", &format!("Bearer {}", t));
        }
        
        // No timeout on SSE - it's a long-lived connection.
        // The server sends heartbeats every 15s to keep it alive.
        // We check the stop flag when reading lines.

        let resp = match req.call() {
            Ok(r) => {
                crate::log!("SSE: connected successfully");
                r
            }
            Err(e) => {
                crate::log!("SSE: connection failed: {}", e);
                if !stop_clone.load(Ordering::Relaxed) {
                    let _ = tx.send(Event::SseError(session_id_clone.clone(), e.to_string()));
                }
                return;
            }
        };

        let reader = BufReader::new(resp.into_reader());
        let mut data_buf = String::new();

        for line in reader.lines() {
            // Check stop flag.
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    if !stop_clone.load(Ordering::Relaxed) {
                        let _ = tx.send(Event::SseError(session_id_clone.clone(), e.to_string()));
                    }
                    break;
                }
            };

            if line.starts_with("data: ") {
                data_buf = line[6..].to_string();
            } else if line.is_empty() && !data_buf.is_empty() {
                // End of event.
                match serde_json::from_str::<SseFrame>(&data_buf) {
                    Ok(frame) => {
                        // Tag the frame with session ID so we can filter.
                        if tx.send(Event::Sse(session_id_clone.clone(), frame)).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        crate::log!("SSE parse error: {} for data: {}", e, &data_buf[..data_buf.len().min(200)]);
                    }
                }
                data_buf.clear();
            }
        }
    });
    
    SseHandle { stop, session_id }
}

/// Handle to stop the global attach thread.
#[derive(Clone)]
pub struct AttachHandle {
    stop: Arc<AtomicBool>,
}

impl AttachHandle {
    /// Signal the attach thread to stop.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Spawn global attach thread to keep server alive.
/// This connects to /attach and stays connected until stopped.
/// Server sends heartbeat pings; we just keep the connection open.
pub fn spawn_attach_thread(base_url: String, token: Option<String>) -> AttachHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();
    
    crate::log!("ATTACH: spawning global attach thread");
    
    thread::spawn(move || {
        let url = format!("{}/attach", base_url);
        crate::log!("ATTACH: connecting to {}", url);
        
        let mut req = ureq::get(&url);
        if let Some(t) = &token {
            req = req.set("Authorization", &format!("Bearer {}", t));
        }
        
        let resp = match req.call() {
            Ok(r) => {
                crate::log!("ATTACH: connected successfully");
                r
            }
            Err(e) => {
                crate::log!("ATTACH: connection failed: {}", e);
                return;
            }
        };
        
        // Just read and discard lines (ping events) until stopped or disconnected.
        let reader = BufReader::new(resp.into_reader());
        for line in reader.lines() {
            if stop_clone.load(Ordering::Relaxed) {
                break;
            }
            if line.is_err() {
                crate::log!("ATTACH: connection lost");
                break;
            }
        }
        crate::log!("ATTACH: thread exiting");
    });
    
    AttachHandle { stop }
}
