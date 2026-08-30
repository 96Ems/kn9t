# Instant Cut — Abort immédiat des streams et tools

**Statut:** Planifié, non implémenté  
**Date:** 2026-08-30  
**Priorité:** Post-v1 (amélioration UX)

---

## Problème

Quand l'utilisateur appuie sur ESC pour abort un turn:
- Le TUI affiche "Aborting..."
- Mais le tool en cours (bash) ou le stream LLM **finit quand même** avant de s'arrêter
- L'utilisateur veut un **cut instantané** (<1ms), pas attendre la fin naturelle

## Cause racine

`Cancel::cancel()` set un flag atomique, mais:
1. Le provider stream bloque sur `read_line()` du socket — ne vérifie jamais le flag
2. L'assembler itère tout le stream sans check cancel
3. Les tools plugin bloquent sur `recv_timeout()` sans polling cancel

## Solution retenue: CancellableReader

Wrapper le body HTTP avec un `CancellableReader` qui vérifie `cancel.cancelled()` à chaque `read()`.

```
Cancel fired → CancellableReader.read() check → Err(Interrupted) → sse_lines EOF → assemble termine
```

**Latence:** < 1ms (une ligne SSE ≈ 100 bytes, le prochain `read()` arrive en µs)

---

## Plan d'implémentation

### Step 1 — CancellableReader (kn9t-provider-core)

**Fichier:** `crates/kn9t-provider-core/src/abort.rs` (nouveau)

```rust
use kn9t_core::Cancel;
use std::io::{self, Read};

/// Wraps a Read stream; returns Err(Interrupted) when cancelled.
pub struct CancellableReader<R> {
    inner: R,
    cancel: Cancel,
}

impl<R> CancellableReader<R> {
    pub fn new(inner: R, cancel: Cancel) -> Self {
        Self { inner, cancel }
    }
}

impl<R: Read> Read for CancellableReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        self.inner.read(buf)
    }
}
```

**Test:** `pcore::cancel_reader_interrupts`

---

### Step 2 — http.rs accepte Cancel

**Fichier:** `crates/kn9t-provider-core/src/http.rs`

```rust
pub fn send(
    req: HttpRequest,
    connect_timeout: Duration,
    cancel: Option<Cancel>,   // ← AJOUT
) -> Result<HttpResponse, ProvErr> {
    // ... existing ...
    
    let body_reader = resp.into_body().into_reader();
    let body: Box<dyn Read + Send> = match cancel {
        Some(c) => Box::new(CancellableReader::new(body_reader, c)),
        None    => Box::new(body_reader),
    };
    
    Ok(HttpResponse { status, headers, body })
}
```

**Note:** Mettre à jour `send_get()` aussi.

---

### Step 3 — OpenAiProvider passe Cancel

**Fichier:** `crates/kn9t-provider-openai/src/provider.rs`

```rust
fn attempt(&self, req: &Request<'_>, model_ref: ModelRef, cancel: Cancel) -> ... {
    let resp = send(http_req, timeout, Some(cancel))?;
    // ...
}

impl Provider for OpenAiProvider {
    fn stream(&self, req: &Request, cancel: &Cancel) -> ... {
        let cancel = cancel.clone();  // utiliser au lieu de _cancel
        with_retry(3, Backoff::default(), || {
            self.attempt(req, model_ref.clone(), cancel.clone())
        })
    }
}
```

**Note:** Ajouter check `cancel.cancelled()` dans `with_retry` entre les retries.

---

### Step 4 — PluginHost polling avec cancel

**Fichier:** `crates/kn9t-plugin/src/host.rs`

Nouvelle méthode:

```rust
pub fn wait_for_streaming_cancellable(
    &self,
    expected_id: u64,
    cancel: &Cancel,
    timeout: Duration,
    mut on_chunk: impl FnMut(serde_json::Value),
) -> Result<Value, String> {
    let rx = self.response_rx.lock().unwrap();
    let deadline = std::time::Instant::now() + timeout;
    
    loop {
        if cancel.cancelled() {
            self.cancel_call(expected_id);  // envoie HostMsg::Cancel au plugin
            return Err("cancelled".to_string());
        }
        
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("plugin '{}' timed out", self.declaration.name));
        }
        
        // Poll court pour permettre check cancel
        let poll_dur = remaining.min(Duration::from_millis(10));
        match rx.recv_timeout(poll_dur) {
            Ok(ReaderMsg::Chunk { id, body }) if id == expected_id => on_chunk(body),
            Ok(ReaderMsg::Final { id, body }) if id == expected_id => return Ok(body),
            Ok(ReaderMsg::Err { id: 0, reason }) => return Err(reason),
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(format!("plugin '{}' disconnected", self.declaration.name));
            }
        }
    }
}
```

---

### Step 5 — RemoteTool cancel listener

**Fichier:** `crates/kn9t-plugin/src/remote_tool.rs`

```rust
impl Tool for RemoteTool {
    fn execute(&self, args: &Value, ctx: &ToolCtx, cancel: &Cancel) -> Result<ToolOutput, ToolErr> {
        let payload = json!({ "tool": self.spec.name, "args": args });
        
        // Pre-assign call ID
        let id = self.host.next_id.fetch_add(1, Ordering::Relaxed);
        
        // Spawn cancel listener thread
        let host = self.host.clone();
        let cancel_clone = cancel.clone();
        let listener = std::thread::spawn(move || {
            while !cancel_clone.cancelled() {
                if cancel_clone.wait_timeout(Duration::from_millis(10)) {
                    host.cancel_call(id);
                    break;
                }
            }
        });
        
        // Call avec ID pré-assigné
        let result = self.host.call_with_id_streaming(
            id, "tool_call", payload, Duration::from_secs(300),
            |chunk| { /* progress */ },
        );
        
        let _ = listener.join();
        // ... reste inchangé ...
    }
}
```

**Requiert:** Nouvelle méthode `PluginHost::call_with_id_streaming()` qui accepte un ID pré-assigné.

---

### Step 6 — Vérification turn.rs

**Fichier:** `crates/kn9t-server/src/turn.rs`

**Aucun changement nécessaire.** L'appel existant `cancel.cancel()` propage maintenant automatiquement:
- → `CancellableReader.read()` retourne `Interrupted`
- → `wait_for_streaming_cancellable()` retourne `Err`
- → Le listener thread envoie `HostMsg::Cancel` au plugin

---

### Step 7 — Test d'acceptance

**Fichier:** `crates/kn9t-provider-core/tests/abort_test.rs`

```rust
#[test]
fn abort_interrupts_sse_stream_quickly() {
    // Fake SSE server: envoie un event puis dort 10 secondes
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        stream.write_all(b"data: {\"x\":1}\n\n").unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_secs(10));
    });
    
    let cancel = Cancel::new();
    let cancel_c = cancel.clone();
    
    // Fire cancel après 50ms
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        cancel_c.cancel();
    });
    
    let start = Instant::now();
    // ... send + read body ...
    let elapsed = start.elapsed();
    
    assert!(elapsed < Duration::from_millis(200));
    server.join().ok();
}
```

---

## Fichiers à modifier

| Fichier | Action |
|---------|--------|
| `kn9t-provider-core/src/abort.rs` | Créer (CancellableReader) |
| `kn9t-provider-core/src/lib.rs` | Re-export |
| `kn9t-provider-core/src/http.rs` | Modifier send() signature |
| `kn9t-provider-openai/src/provider.rs` | Passer cancel à send() |
| `kn9t-plugin/src/host.rs` | Ajouter wait_for_streaming_cancellable() |
| `kn9t-plugin/src/remote_tool.rs` | Cancel listener thread |
| Tests | Nouveau test abort timing |

---

## Risques

| Risque | Sévérité | Mitigation |
|--------|----------|------------|
| ureq buffer 4KB → délai max = temps pour fill 4KB | Faible | SSE lines courtes; < 5ms WAN |
| with_retry re-tente après cancel | Moyen | Check cancel dans boucle retry |
| Cancel listener thread leak | Faible | Sort au prochain poll (10ms max) |

---

## Estimation

| Step | Temps |
|------|-------|
| 1-3 (HTTP layer) | 2h |
| 4-5 (Plugin layer) | 3h |
| 6-7 (Tests) | 1h |
| **Total** | ~6h |
