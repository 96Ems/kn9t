# Detailed Duplication Patterns & Dead Code Analysis

## 1. EVENT CONVERSION BOILERPLATE (High-Priority Duplication)

### Location
**File**: `crates/kn9t-core/src/event.rs:439-517`  
**Lines**: 79 lines of repetitive field mapping

### Problem
```rust
// CURRENT: Manual field-by-field mapping
impl From<LiveEvent> for Event {
    fn from(live: LiveEvent) -> Self {
        match live {
            LiveEvent::TurnStarted { turn } => Event::TurnStarted { turn },
            LiveEvent::TextDelta { msg_id, idx, delta } => Event::TextDelta { msg_id, idx, delta },
            LiveEvent::ThinkingDelta { msg_id, idx, delta } => {
                Event::ThinkingDelta { msg_id, idx, delta }
            }
            // ... repeated 30+ times
            LiveEvent::Error { message } => Event::Error { message },
        }
    }
}
```

**Issues**:
- Same variants exist in both LiveEvent and Event (duplication)
- Every new event variant requires manual update in 3 places:
  1. Event enum definition
  2. LiveEvent enum definition  
  3. Conversion impl block
- Error-prone: forgetting one cause hard-to-debug issues
- 79 lines for something that could be derived

### Root Cause
LiveEvent is meant to filter Event down to transient-only variants. But this is enforced at runtime, not compile-time.

### Solution Options

**Option A: Flatten to Single Enum** (Most Radical)
```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    // Durable variants
    #[serde(skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    
    // ... rest of variants
}

// Only allow live emission through:
pub fn emit_live(event: Event) -> Result<(), String> {
    if event.seq().is_some() {
        Err("cannot emit durable event through live path".into())
    }
    Ok(())
}
```
**Pros**: Single source of truth  
**Cons**: Loses compile-time safety

**Option B: Macro-Driven Conversion** (Recommended)
```rust
macro_rules! event_from_live {
    ($($variant:ident { $($field:ident),* $(,)? }),*) => {
        impl From<LiveEvent> for Event {
            fn from(live: LiveEvent) -> Self {
                match live {
                    $(
                        LiveEvent::$variant { $($field),* } => 
                            Event::$variant { $($field),* },
                    )*
                }
            }
        }
    }
}

event_from_live!(
    TurnStarted { turn },
    TextDelta { msg_id, idx, delta },
    // ... all transient variants
);
```
**Pros**: Maintainable, still compile-time safe  
**Cons**: Macro complexity

**Option C: Use derive-based Solutions**
Use crates like `strum` or manual impl that reduce boilerplate:
```rust
pub trait IntoEvent: Sized {
    fn into_event(self) -> Event;
}
```
**Pros**: Clean API  
**Cons**: Requires trait implementation

### Recommendation
**Use Option B (Macro-Driven)** for this specific case - it preserves compile-time checking while eliminating duplication. Or standardize all similar conversions on a macro pattern.

**Estimated Implementation**: 2-3 hours  
**Risk**: Low (purely mechanical)  
**Savings**: -50 lines, +0 functionality

---

## 2. SSE PROTOCOL DUPLICATION (Critical - 4 Locations)

### Affected Files

#### 1a. `crates/kn9t-server/src/sse.rs` (314 lines)
```rust
// EventSource streaming, chunk parsing
pub async fn server_sent_events<...>(
    // Sets up SSE stream
    // Parses incoming chunks
)
```

#### 1b. `crates/kn9t-plugin-sdk/src/sse.rs` (137 lines)
```rust
// Plugin-side SSE handling
pub fn read_chunk<R: Read>(reader: &mut R) -> Result<String, SseError> {
    // Frame parsing logic
}
```

#### 1c. `crates/kn9t-provider-core/src/sse.rs` (34 lines)
```rust
// Minimal SSE support
pub enum SseEvent { /* ... */ }
```

#### 1d. `crates/kn9t-provider-replay/src/sse.rs` (155 lines)
```rust
// Replay-specific SSE handling
// Record/playback of SSE streams
```

### Analysis

**Shared Patterns**:
1. SSE frame parsing (header parsing, newline handling)
2. Chunk accumulation (buffers, flush logic)
3. Reconnection logic (retry, backoff)
4. Error handling (protocol violations)

**Differences**:
- Provider implementations handle streaming responses
- Plugin SDK handles request/response pairs
- Replay has record/playback layer
- Server is async, others might be sync

### Current Implementation Risks
```
High Risk Zone - Each has slightly different behavior:
┌─────────────────────┬──────────────────┬────────────────┐
│ Location            │ Parsing Logic    │ Behavior       │
├─────────────────────┼──────────────────┼────────────────┤
│ server/sse.rs       │ Custom async     │ ??? (check)    │
│ plugin-sdk/sse.rs   │ Custom sync      │ ??? (check)    │
│ provider-core/sse.rs│ Minimal          │ ??? (check)    │
│ provider-replay/sse │ Custom + record  │ ??? (check)    │
└─────────────────────┴──────────────────┴────────────────┘

If one has a bug fix, other 3 need review!
```

### Consolidation Plan

**Create new crate: `kn9t-sse`**

```rust
// crates/kn9t-sse/Cargo.toml
[package]
name = "kn9t-sse"
version = "0.1.0"

[dependencies]
serde_json = "1.0"
thiserror = "1.0"
tokio = { version = "1.0", optional = true, features = ["io-util"] }

[features]
async = ["tokio"]
```

**Structure**:
```rust
// crates/kn9t-sse/src/lib.rs
pub mod frame;      // SSE frame parsing (core)
pub mod stream;     // Streaming helpers
pub mod error;      // SSE-specific errors
pub mod codec;      // Encoding/decoding

// For async consumers
#[cfg(feature = "async")]
pub mod async_stream;
```

**Frame Module** (`frame.rs`):
```rust
/// SSE frame format:
/// ```
/// field: value\r\n
/// field: value\r\n
/// \r\n
/// ```
pub struct Frame {
    pub fields: HashMap<String, String>,
}

impl Frame {
    pub fn parse(line: &str) -> Result<Option<Self>, SseError> {
        // Unified parsing logic
    }
    
    pub fn get_data(&self) -> Option<&str> {
        self.fields.get("data").map(|s| s.as_str())
    }
}
```

**Stream Module** (`stream.rs`):
```rust
/// Stateful SSE stream reader
pub struct SseReader<R> {
    reader: R,
    buffer: String,
}

impl<R: Read> SseReader<R> {
    pub fn read_frame(&mut self) -> Result<Option<Frame>, SseError> {
        // Unified implementation
    }
    
    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

#[cfg(feature = "async")]
pub struct AsyncSseReader<R> {
    reader: R,
    buffer: String,
}
```

**Error Module** (`error.rs`):
```rust
#[derive(Debug, thiserror::Error)]
pub enum SseError {
    #[error("Invalid SSE frame: {0}")]
    InvalidFrame(String),
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("UTF-8 error: {0}")]
    Utf8(#[from] std::string::FromUtf8Error),
}
```

### Migration Steps

**Phase 1: Create kn9t-sse**
1. Create new crate
2. Extract common frame parsing logic
3. Add to workspace Cargo.toml
4. Add comprehensive tests

**Phase 2: Migrate Provider**
1. Update `kn9t-provider-core/Cargo.toml` to depend on `kn9t-sse`
2. Replace custom parsing with `kn9t_sse::frame::parse()`
3. Remove `sse.rs`, keep only integration if needed
4. Test provider streaming

**Phase 3: Migrate Plugin SDK**
1. Update dependencies
2. Replace sync reader with `SseReader`
3. Test plugin protocol

**Phase 4: Migrate Server**
1. Create async wrapper or use `tokio::io` compatible interface
2. Replace custom async SSE logic
3. Test server streaming

**Phase 5: Migrate Replay**
1. Adapt record/playback layer on top of common frame logic
2. Consolidate parsing

### Expected Consolidation
- **Deleted**: ~400 lines of duplicated parsing
- **Added**: ~150 lines of consolidated parsing + tests
- **Net Savings**: ~250 lines
- **Benefit**: Single source of truth for SSE protocol

### Implementation Timeline
- **Phase 1**: 4 hours (create crate, extract core)
- **Phase 2-5**: 6 hours (migrate, test, verify)
- **Total**: 10 hours, 1-2 days effort

---

## 3. HTTP CLIENT CONFIGURATION DUPLICATION

### Affected Files

#### 3a. `crates/kn9t/src/http.rs` (315 lines)
```rust
// CLI HTTP client setup
pub fn create_http_client() -> Result<reqwest::Client, Box<dyn Error>> {
    // Timeout setup
    // Retry configuration
    // Auth headers
    // User-agent
}
```

#### 3b. `crates/kn9t-server/src/http_util.rs` (203 lines)
```rust
// Server HTTP utilities
pub fn http_client_builder() -> reqwest::ClientBuilder {
    // Similar timeout
    // Similar retry logic
    // Similar headers
}
```

#### 3c. `crates/kn9t-provider-core/src/http.rs` (142 lines)
```rust
// Provider HTTP setup
pub struct HttpClientConfig {
    timeout: Duration,
    retry: RetryConfig,
}
```

### Duplication Details

**Timeout Configuration**:
```rust
// in crates/kn9t/src/http.rs
let timeout = Duration::from_secs(300);

// in crates/kn9t-server/src/http_util.rs
let timeout = Duration::from_secs(300);  // SAME

// in crates/kn9t-provider-core/src/http.rs
let timeout = Duration::from_secs(300);  // SAME
```

**Retry Logic**:
```rust
// Appears to implement similar exponential backoff in all 3 places
// But with slight variations that could diverge
```

### Consolidation Plan

**In `crates/kn9t-provider-core/src/http.rs`**:
```rust
/// Unified HTTP client configuration
pub struct HttpClientBuilder {
    timeout: Duration,
    retry_config: RetryConfig,
    auth_token: Option<String>,
    user_agent: String,
}

impl HttpClientBuilder {
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(300),  // Standard timeout
            retry_config: RetryConfig::default(),
            auth_token: None,
            user_agent: "kn9t/1.0".to_string(),
        }
    }
    
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    
    pub fn with_auth(mut self, token: String) -> Self {
        self.auth_token = Some(token);
        self
    }
    
    pub fn build(self) -> Result<reqwest::Client, Box<dyn Error>> {
        let mut builder = reqwest::Client::builder()
            .timeout(self.timeout)
            .user_agent(&self.user_agent);
        
        if let Some(token) = self.auth_token {
            builder = builder.default_headers(
                HeaderMap::from_iter([(
                    AUTHORIZATION,
                    format!("Bearer {}", token).parse()?,
                )])
            );
        }
        
        Ok(builder.build()?)
    }
}

pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay: Duration,
    pub backoff_multiplier: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            backoff_multiplier: 2.0,
        }
    }
}
```

### Usage After Consolidation

**In kn9t CLI** (`crates/kn9t/src/http.rs`):
```rust
let client = HttpClientBuilder::new()
    .with_auth(token)
    .build()?;
```

**In server** (`crates/kn9t-server/src/http_util.rs`):
```rust
let client = HttpClientBuilder::new()
    .with_timeout(Duration::from_secs(600))
    .build()?;
```

**Deleted files**:
- Most of `crates/kn9t/src/http.rs` (keep wrapper if needed)
- Most of `crates/kn9t-server/src/http_util.rs` (keep high-level API)

### Implementation Timeline
- **Phase 1**: 2 hours (extract builder to provider-core)
- **Phase 2**: 2 hours (update CLI)
- **Phase 3**: 2 hours (update server)
- **Total**: 6 hours, <1 day effort

**Savings**: ~200 lines consolidated, single source of truth

---

## 4. WIRE PROTOCOL POTENTIAL DUPLICATION

### Affected Files

#### 4a. `crates/kn9t-plugin-sdk/src/wire.rs` (282 lines)
```rust
/// Plugin wire protocol messages
pub enum PluginMsg {
    Call { id: u64, name: String, args: Value },
    Done { id: u64, result: Value },
    // ...
}
```

#### 4b. `crates/kn9t-tui/src/wire.rs` (292 lines)
```rust
/// TUI wire protocol messages
pub enum Message {
    SessionStart { id: String },
    EventUpdate { id: String, event: Value },
    // ...
}
```

### Assessment

**Status**: ⚠️ NEEDS REVIEW - Potentially DUPLICATE but may be intentional

**Questions**:
1. Are these intended to be different?
2. Is TUI importing from SDK or reimplementing?
3. Are message types truly identical or different by design?

### Recommendation

**Action Item**: 
```rust
// In kn9t-tui/src/wire.rs check:
// 1. Are variants identical to kn9t-plugin-sdk/src/wire.rs?
// 2. Is there import of SDK wire?
// 3. If different, document why
// 4. If same, consolidate
```

**If consolidation is possible**:
- Move to shared module
- Remove duplication
- Saves ~150 lines

---

## 5. UNUSED/STUB CODE CANDIDATES

### 5a. Empty Cache File
**File**: `crates/kn9t-provider-openai/src/cache.rs`  
**Lines**: 14  
**Content**: Nearly empty

```rust
//! Caching layer for OpenAI provider.

// Note: Would implement caching logic here
```

**Status**: Likely placeholder or WIP  
**Action**: Remove if not used  
**Savings**: 14 lines

### 5b. Minimal SSE Implementation
**File**: `crates/kn9t-provider-core/src/sse.rs` (34 lines)  
**Content**: Just type definition

**Status**: Overshadowed by more complete implementations  
**Action**: Merge into consolidated SSE crate  
**Savings**: Handled by SSE consolidation

---

## 6. TRAIT BOUND ANALYSIS

### Pattern: Potentially Over-Constrained Generics

**Example 1** - `crates/kn9t-plugin-sdk/src/traits.rs`
```rust
pub trait Handler<T: Serialize + Deserialize + Send + Sync> {
    // Might not need all bounds
}
```

**Recommendation**: 
- Audit which bounds are actually used
- Use `clippy::trait_bounds` lint
- Document bounds in docs if minimal

### Pattern: Unused Type Parameters

**Example 2** - Search for `#[allow(unused)]` annotations
```rust
#[allow(unused_variables)]
fn handler(unused_param: String) {
    // Why is it unused?
}
```

**Recommendation**:
- Remove unused params
- If needed for API compatibility, document

---

## 7. CONDITIONAL CODE PATH COVERAGE

### Pattern: Platform-Specific Code
**File**: `crates/kn9t-server/src/tools.rs:57-71`
```rust
#[cfg(target_family = "unix")]
{
    // Unix plugin detection
}
#[cfg(target_family = "windows")]
{
    // Windows plugin detection
}
```

**Status**: ✅ Good pattern, but ensure:
- [ ] Both paths tested
- [ ] CI runs both Windows and Unix builds
- [ ] No untested platform paths

---

## 8. RECOMMENDED CLEANUP ORDER

### Tier 1: Quick Wins (< 4 hours)
- [ ] Remove unused stubs (cache.rs)
- [ ] Document wire.rs duplication intent
- [ ] Add `#[must_use]` attributes to core functions

### Tier 2: Medium Effort (4-8 hours)
- [ ] Consolidate HTTP client → provider-core
- [ ] Consolidate SSE → new crate
- [ ] Macro-ify Event ↔ LiveEvent conversion

### Tier 3: Structural (8-20 hours)
- Large file splits documented in CLEANUP_CHECKLIST.md

---

## VALIDATION COMMANDS

After implementing any consolidation:

```bash
# Verify no broken imports
cargo check --all

# Ensure no unused code
cargo clippy --all -- -W clippy::unused_imports

# Run all tests
cargo test --all

# Check documentation
cargo doc --no-deps

# Verify size reduction
find crates -name "*.rs" | xargs wc -l | tail -1
# Should show reduced total
```

---

*This document provides detailed analysis for developers implementing cleanup. Use alongside CLEANUP_CHECKLIST.md for action items.*
