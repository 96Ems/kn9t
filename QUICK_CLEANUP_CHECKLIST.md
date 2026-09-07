# QUICK CLEANUP CHECKLIST & ACTION ITEMS

## 🎯 PHASE 1 - MONOLITHIC FILES SPLIT (Week 1)

### ✅ Task 1.1: Split kn9t-tui/app.rs (4170 LOC)
**Current:** Single 4170-line state machine + event handler  
**Target:** 5 focused modules, ~700-800 LOC each

**Branch Name:** `refactor/tui-app-split`

**Files to Create:**
- [ ] `crates/kn9t-tui/src/app/state.rs` - App state struct only (150 LOC)
- [ ] `crates/kn9t-tui/src/app/init.rs` - Initialization logic (200 LOC)
- [ ] `crates/kn9t-tui/src/app/sse_handler.rs` - SSE event processing (500 LOC)
- [ ] `crates/kn9t-tui/src/app/tool_executor.rs` - Tool state machine (300 LOC)
- [ ] `crates/kn9t-tui/src/app/mod.rs` - Module coordinator (100 LOC)

**Files to Modify:**
- [ ] `crates/kn9t-tui/src/app.rs` → Becomes thin wrapper to app/mod.rs

**Tests:**
- [ ] Verify `cargo test --lib app` passes
- [ ] Check UI still responds to keyboard input
- [ ] Validate SSE event processing

**Verification:**
```bash
cargo clippy -p kn9t-tui
cargo test -p kn9t-tui --lib
```

---

### ✅ Task 1.2: Split kn9t-tui/reducer.rs (1559 LOC)
**Current:** Massive match statement on all action types  
**Target:** 5 domain-specific reducer modules

**Branch Name:** `refactor/tui-reducer-split`

**Files to Create:**
- [ ] `crates/kn9t-tui/src/reducers/mod.rs` - Dispatcher (100 LOC)
- [ ] `crates/kn9t-tui/src/reducers/session.rs` - Session actions (150 LOC)
- [ ] `crates/kn9t-tui/src/reducers/prompt.rs` - Input/prompt actions (150 LOC)
- [ ] `crates/kn9t-tui/src/reducers/ui.rs` - Visual state changes (200 LOC)
- [ ] `crates/kn9t-tui/src/reducers/message.rs` - Message list (200 LOC)
- [ ] `crates/kn9t-tui/src/reducers/tool.rs` - Tool state (200 LOC)
- [ ] `crates/kn9t-tui/src/reducers/search.rs` - Search state (150 LOC)

**Files to Modify:**
- [ ] `crates/kn9t-tui/src/reducer.rs` → Delete (move to mod.rs)
- [ ] `crates/kn9t-tui/src/lib.rs` → Update imports

**Tests:**
- [ ] Unit tests for each reducer
- [ ] Acceptance test: send action → verify state change

**Verification:**
```bash
cargo test -p kn9t-tui reducers
cargo clippy -p kn9t-tui
```

---

### ✅ Task 1.3: Create kn9t-core::http_util (Consolidate 3 files)
**Current:** 
- `kn9t/http.rs` (315 LOC)
- `kn9t-server/http_util.rs` (203 LOC)
- `kn9t-provider-core/http.rs` (297 LOC)

**Target:** Single shared module in kn9t-core

**Branch Name:** `refactor/http-consolidation`

**Implementation:**
- [ ] Create `crates/kn9t-core/src/http_util.rs` (~250 LOC, best of 3)
- [ ] Export from `crates/kn9t-core/src/lib.rs`
- [ ] Update `kn9t/Cargo.toml` to depend on kn9t-core (if not already)
- [ ] Replace `kn9t/http.rs` with re-export: `pub use kn9t_core::http_util::*;`
- [ ] Replace `server/http_util.rs` with re-export
- [ ] Replace `provider-core/http.rs` with re-export

**Functions to Consolidate:**
- `connect(host, who) -> TcpStream`
- `parse_body(response) -> Value`
- `read_all(stream) -> String`

**Tests:**
- [ ] `cargo test -p kn9t-core http_util`
- [ ] Verify CLI still works: `kn9t chat "test"`
- [ ] Verify server still works: `cargo test -p kn9t-server`

---

### ✅ Task 1.4: Split kn9t-server/config.rs (1231 LOC)
**Current:** Mixed TOML parsing + model registry + policy + quirks  
**Target:** Coordinator + 3 focused modules

**Branch Name:** `refactor/server-config-split`

**Files to Create:**
- [ ] `crates/kn9t-server/src/config/mod.rs` - Coordinator (200 LOC)
- [ ] `crates/kn9t-server/src/config/provider.rs` - ProviderConfig (150 LOC)
- [ ] `crates/kn9t-server/src/config/models.rs` - ModelRegistry (300 LOC)
- [ ] `crates/kn9t-server/src/config/tools.rs` - ToolConfig (200 LOC)
- [ ] `crates/kn9t-server/src/config/policy.rs` - PolicyConfig (200 LOC)

**Files to Modify:**
- [ ] `crates/kn9t-server/src/config.rs` → Becomes `crates/kn9t-server/src/config.rs` (KEEP as re-export)
- [ ] `crates/kn9t-server/src/lib.rs` → Update pub mod config

**Tests:**
- [ ] `cargo test -p kn9t-server config`
- [ ] Load config from examples: `RUST_LOG=debug cargo run -p kn9t-server`
- [ ] Verify model loading works

---

## 🎯 PHASE 2 - MEDIUM REFACTORS (Week 2)

### ✅ Task 2.1: Split kn9t-tui/ui/render.rs (3000+ LOC)
**Current:** Single massive render function  
**Target:** 8 focused panel/layout modules

**Branch Name:** `refactor/tui-render-split`

**Files to Create:**
- [ ] `crates/kn9t-tui/src/ui/render/mod.rs` - Main coordinator
- [ ] `crates/kn9t-tui/src/ui/render/message_panel.rs` (600 LOC)
- [ ] `crates/kn9t-tui/src/ui/render/input_panel.rs` (400 LOC)
- [ ] `crates/kn9t-tui/src/ui/render/help_panel.rs` (400 LOC)
- [ ] `crates/kn9t-tui/src/ui/render/tools_panel.rs` (300 LOC)
- [ ] `crates/kn9t-tui/src/ui/render/status_bar.rs` (200 LOC)
- [ ] `crates/kn9t-tui/src/ui/render/layout.rs` - Enhance existing (300 LOC)
- [ ] `crates/kn9t-tui/src/ui/render/styles.rs` (400 LOC)

**Files to Modify:**
- [ ] `crates/kn9t-tui/src/ui/render.rs` → Becomes directory

**Testing:**
- [ ] Compile & run: `cargo run -p kn9t-tui --example interactive`
- [ ] Visual inspection: panels render correctly
- [ ] Terminal width/height changes handled

---

### ✅ Task 2.2: Refactor kn9t-server/tools.rs (822 LOC)
**Current:** All tool implementations in one file  
**Target:** Tool executor + individual tool modules

**Branch Name:** `refactor/server-tools-split`

**Files to Create:**
- [ ] `crates/kn9t-server/src/tools/mod.rs` - Coordinator/executor
- [ ] `crates/kn9t-server/src/tools/bash.rs` - bash tool
- [ ] `crates/kn9t-server/src/tools/read.rs` - file reading
- [ ] `crates/kn9t-server/src/tools/write.rs` - file writing
- [ ] `crates/kn9t-server/src/tools/edit.rs` - file editing
- [ ] `crates/kn9t-server/src/tools/search.rs` - search/grep tools
- [ ] `crates/kn9t-server/src/tools/result.rs` - **SHARED** result formatting

**Files to Modify:**
- [ ] `crates/kn9t-server/src/tools.rs` → Coordinator

**Export result formatter to CLI/TUI:**
- [ ] Add `pub use tools::result::*;` in kn9t-core or server lib
- [ ] Update CLI chat.rs to import from there
- [ ] Update TUI message_handler.rs to import from there

---

### ✅ Task 2.3: Consolidate Acceptance Tests
**Current:**
- `kn9t-core/tests/acceptance.rs` (18.8 KB)
- `kn9t-server/tests/acceptance.rs` (147.6 KB) ← GIANT
- `kn9t-plugin/tests/acceptance.rs` (56.8 KB)
- `kn9t-tui/tests/acceptance.rs` (15.8 KB)
- `kn9t-react/tests/acceptance.rs` (82.5 KB)

**Target:** 
- `/tests/acceptance/mod.rs` - Shared fixtures/helpers
- `/tests/acceptance/server_tests.rs` (100 KB)
- `/tests/acceptance/plugin_tests.rs` (50 KB)
- `/tests/acceptance/client_tests.rs` (50 KB)

**Branch Name:** `refactor/consolidate-tests`

**Implementation:**
- [ ] Create workspace tests directory
- [ ] Move fixtures to `tests/fixtures/`
- [ ] Consolidate test helpers
- [ ] Reduce server acceptance test by 30% (remove duplication)

---

### ✅ Task 2.4: Extract kn9t-core::backoff module
**Current:** Duplicated in:
- `kn9t/chat.rs:663` - `acquire_lease_with_backoff()` (17 LOC)
- `kn9t-tui/client.rs` - backoff logic (~40 LOC)

**Branch Name:** `refactor/backoff-util`

**Implementation:**
- [ ] Create `crates/kn9t-core/src/backoff.rs` (50 LOC)
- [ ] Define `BackoffRetry` trait/struct
- [ ] Update CLI and TUI to use it
- [ ] Document in README with example

---

## 🎯 PHASE 3 - CLEANUP & DEAD CODE (Week 3)

### ✅ Task 3.1: Run Clippy & Fix Warnings
```bash
cargo clippy --all --all-targets --all-features -- -D warnings
```

**Branch Name:** `refactor/clippy-fixes`

**Actions:**
- [ ] Remove unused imports
- [ ] Address `allow(dead_code)` attributes
- [ ] Fix clippy lints

---

### ✅ Task 3.2: Consolidate SSE Logic
**Current:**
- `kn9t-core/event.rs` (502 LOC)
- `kn9t-server/sse.rs` (314 LOC)
- `kn9t-plugin-sdk/sse.rs` (150 LOC)
- `kn9t-provider-core/http.rs` - SSE parsing

**Branch Name:** `refactor/sse-consolidation`

**Implementation:**
- [ ] Define SSE format spec in `kn9t-core/sse_codec.rs`
- [ ] Consolidate parsing logic
- [ ] Create trait for SSE serializable events
- [ ] Update all layers to use shared codec

---

### ✅ Task 3.3: Create ARCHITECTURE.md
**Location:** Repository root  
**Content:**
- Crate dependency diagram
- Module organization overview
- Plugin vs Provider explanation
- Data flow diagram (CLI → Server → DB)

**Branch Name:** `docs/architecture`

---

### ✅ Task 3.4: Update Module READMEs
- [ ] `crates/kn9t/README.md` - CLI launcher responsibilities
- [ ] `crates/kn9t-server/README.md` - Server architecture
- [ ] `crates/kn9t-plugin/README.md` - Plugin system
- [ ] `crates/kn9t-tui/README.md` - TUI architecture

---

## 🎯 PHASE 4 - VERIFICATION (Week 4)

### ✅ Final Validation Checklist

```bash
# 1. Full build
cargo build --release --all 2>&1 | tee build.log

# 2. All tests pass
cargo test --all --release 2>&1 | tee test.log

# 3. No clippy warnings
cargo clippy --all --all-targets -- -D warnings 2>&1 | tee clippy.log

# 4. Code formatting
cargo fmt --all -- --check 2>&1 | tee fmt.log

# 5. No duplicate dependencies
cargo tree -d 2>&1 | tee deps.log

# 6. Benchmark comparison (if applicable)
cargo bench --all 2>&1 | tee bench-after.log

# 7. Manual acceptance tests
# - Run: kn9t chat "hello"
# - Run: kn9t-tui (interactive)
# - Test plugin loading: kn9t install-plugins

# 8. Size comparison
ls -lh target/release/{kn9t,kn9t-server,kn9t-tui}
```

---

## 📊 EFFORT TRACKING

| Phase | Task | Est. Hours | Status |
|-------|------|-----------|--------|
| 1 | app.rs split | 8 | [ ] |
| 1 | reducer.rs split | 6 | [ ] |
| 1 | http consolidation | 4 | [ ] |
| 1 | config.rs split | 6 | [ ] |
| 2 | render.rs split | 8 | [ ] |
| 2 | tools.rs split | 6 | [ ] |
| 2 | test consolidation | 4 | [ ] |
| 2 | backoff extraction | 3 | [ ] |
| 3 | Clippy fixes | 3 | [ ] |
| 3 | SSE consolidation | 5 | [ ] |
| 3 | Architecture docs | 4 | [ ] |
| 3 | Module READMEs | 4 | [ ] |
| 4 | Verification | 4 | [ ] |
| **Total** | | **65 hours** | |

**Recommended:** 1 developer full-time for 2-3 weeks OR 2 developers in parallel on phases 1-2.

---

## ✨ SUCCESS CRITERIA

- [ ] All phases complete
- [ ] Zero clippy warnings
- [ ] All tests passing (cargo test --all)
- [ ] Build time ±10% of baseline
- [ ] No breaking changes to public API
- [ ] ARCHITECTURE.md written and reviewed
- [ ] All module READMEs updated
- [ ] Code review approved by tech lead

---

## 🚀 GO/NO-GO DECISION

**Before starting Phase 1, confirm:**

- [ ] Team alignment on approach
- [ ] No concurrent major features being merged
- [ ] Sufficient developer time allocated
- [ ] CI/CD pipeline stable
- [ ] Release branch protected during refactor

---

**Last Updated:** 2026-09-07  
**Report:** CODEBASE_CLEANUP_REPORT.md  
**Detailed Findings:** DUPLICATION_AND_DEAD_CODE_DETAILS.md
