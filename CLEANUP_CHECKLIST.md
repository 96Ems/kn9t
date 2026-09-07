# Code Cleanup Action Items Checklist

## 🔴 CRITICAL PRIORITY (Address First)

### [ ] 1. Split `crates/kn9t-tui/src/app.rs` (4189 lines)
**Complexity**: 🔥🔥🔥🔥 Very High  
**Impact**: Massive (currently 6.4% of entire codebase)

**New File Structure**:
```
crates/kn9t-tui/src/app/
├── mod.rs                    (keep state struct, re-exports)
├── core.rs                   (AppState, initialization)
├── events.rs                 (event dispatch, handlers)
├── render.rs                 (rendering pipeline)
├── session.rs                (session management)
├── input.rs                  (keyboard/mouse input)
└── hooks.rs                  (lifecycle hooks)
```

**Steps**:
1. Extract structs without methods to new files first
2. Move handler functions (currently massive match arms)
3. Keep AppState in mod.rs
4. Update imports across kn9t-tui
5. Test thoroughly - this is the main TUI hub

**Estimated Time**: 3-4 days  
**Risk**: High (central component)  
**Test Coverage Needed**: All TUI integration tests

---

### [ ] 2. Split `crates/kn9t-tui/src/ui/render.rs` (4108 lines)
**Complexity**: 🔥🔥🔥🔥 Very High  
**Impact**: Massive (this is PURE rendering, needs careful extraction)

**New File Structure**:
```
crates/kn9t-tui/src/ui/render/
├── mod.rs                   (Renderer trait, main dispatcher)
├── message.rs               (message rendering)
├── diff.rs                  (diff viewer rendering)
├── widget.rs                (widget composition)
├── syntax.rs                (syntax highlight application)
├── theme.rs                 (theme application, colors)
└── markdown.rs              (markdown-specific rendering)
```

**Challenges**:
- Heavy use of shared state/context
- Multiple trait implementations
- Complex color/style application pipeline
- Performance-critical code

**Steps**:
1. Identify pure rendering functions
2. Extract widget render functions first (easiest)
3. Then message/diff rendering (medium)
4. Keep Renderer trait in mod.rs
5. Profile performance after split

**Estimated Time**: 4-5 days  
**Risk**: High (performance regression possible)  
**Test Coverage Needed**: Visual regression tests, benchmark suite

---

### [ ] 3. Consolidate SSE Protocol Implementations
**Complexity**: 🔥🔥 Medium-High  
**Files Affected**: 4 crates

**Location**: 
- `crates/kn9t-server/src/sse.rs` (314 lines)
- `crates/kn9t-plugin-sdk/src/sse.rs` (137 lines)
- `crates/kn9t-provider-core/src/sse.rs` (34 lines)
- `crates/kn9t-provider-replay/src/sse.rs` (155 lines)

**New Structure**: Create `crates/kn9t-sse/`
```
kn9t-sse/src/
├── lib.rs              (re-exports)
├── frame.rs            (SSE frame parsing)
├── stream.rs           (streaming logic)
├── error.rs            (SSE-specific errors)
└── codec.rs            (encoding/decoding)
```

**Implementation Steps**:
1. Analyze each SSE implementation for differences
2. Document which have correct behavior
3. Create common trait for SSE handling
4. Migrate each crate to use common crate
5. Add comprehensive SSE tests

**Estimated Time**: 2-3 days  
**Risk**: Medium (potential behavior differences)  
**Test Coverage Needed**: Unit tests for each protocol variant

---

## 🟡 HIGH PRIORITY (Do Soon)

### [ ] 4. Split `crates/kn9t-tui/src/lua/mod.rs` (1427 lines)
**Complexity**: 🔥🔥🔥 High  
**Files**: Single mega-module needs breaking up

**Structure**:
```
crates/kn9t-tui/src/lua/
├── mod.rs              (LuaRuntimeManager, exports)
├── vm.rs               (VM initialization, config)
├── api/
│   ├── mod.rs          (API registry)
│   ├── ui.rs           (UI API bindings)
│   ├── state.rs        (state access API)
│   └── event.rs        (event API)
└── sandbox.rs          (keep as-is, already isolated)
```

**Steps**:
1. Identify Lua binding functions (likely 60% of file)
2. Group by API category
3. Extract initialization
4. Move to separate files
5. Validate via Lua tests

**Estimated Time**: 2-3 days  
**Risk**: Medium (Lua integration is fragile)  
**Test Coverage Needed**: All Lua API tests

---

### [ ] 5. Refactor `crates/kn9t-tui/src/reducer.rs` (1559 lines)
**Complexity**: 🔥🔥 Medium-High  
**Pattern**: Single giant match statement needs breaking up

**Structure**:
```
crates/kn9t-tui/src/reducer/
├── mod.rs              (reducer dispatch, main match)
├── session.rs          (session events)
├── input.rs            (input events)
├── render.rs           (render events)
├── search.rs           (search events)
├── ui.rs               (UI state events)
└── helpers.rs          (shared helper functions)
```

**Implementation**:
1. Create `reducer/` directory
2. Extract handler functions to separate files
3. Keep dispatcher in mod.rs
4. Group by event type, not implementation
5. Test each reducer separately

**Estimated Time**: 2 days  
**Risk**: Low-Medium (well-isolated state machine)  
**Test Coverage Needed**: Unit tests for each reducer

---

### [ ] 6. Split `crates/kn9t-server/src/config.rs` (1231 lines)
**Complexity**: 🔥🔥 Medium  
**Pattern**: Mixed concerns - parsing, validation, defaults

**Structure**:
```
crates/kn9t-server/src/config/
├── mod.rs              (ResolvedConfig, parsing)
├── policy.rs           (policy config, R-CORE-240 stuff)
├── plugin.rs           (plugin discovery, setup)
├── model.rs            (model configuration)
├── loader.rs           (TOML file loading)
└── defaults.rs         (default configurations)
```

**Extraction Order**:
1. Extract policy configuration (~400 lines)
2. Extract plugin configuration (~200 lines)
3. Extract model configuration (~150 lines)
4. Keep loader/defaults in main config.rs

**Estimated Time**: 1-2 days  
**Risk**: Low (mostly parsing, easy to unit test)  
**Test Coverage Needed**: Config parsing tests, backward compat tests

---

### [ ] 7. Consolidate HTTP Client Setup
**Complexity**: 🔥 Medium  
**Files Affected**: 3 locations

**Current Locations**:
- `crates/kn9t/src/http.rs` (315 lines) - CLI HTTP
- `crates/kn9t-server/src/http_util.rs` (203 lines) - Server HTTP
- `crates/kn9t-provider-core/src/http.rs` (142 lines) - Provider HTTP

**Plan**:
1. Audit each for unique vs. shared logic
2. Extract shared builder to `kn9t-provider-core`
3. Create `HttpClientBuilder` with:
   - Timeout configuration
   - Retry logic (with exponential backoff)
   - Header defaults
   - Auth handling
   - Tracing/logging
4. Replace duplicates with builder usage
5. Consolidate retry logic

**Estimated Time**: 1 day  
**Risk**: Low (isolated HTTP setup)  
**Test Coverage Needed**: HTTP client tests, retry logic tests

---

### [ ] 8. Split `crates/kn9t-plugin/src/host.rs` (1183 lines)
**Complexity**: 🔥🔥 Medium-High  
**Pattern**: Process management + IPC protocol + state machine

**Structure**:
```
crates/kn9t-plugin/src/host/
├── mod.rs              (PluginHost, main API)
├── process.rs          (process lifecycle)
├── protocol.rs         (IPC protocol handling)
├── state.rs            (host state machine)
└── error.rs            (host-specific errors)
```

**Steps**:
1. Extract process spawning logic
2. Extract protocol frame handling
3. Keep host state in mod.rs
4. Update tests accordingly
5. Ensure error handling stays cohesive

**Estimated Time**: 2 days  
**Risk**: Medium (process management is tricky)  
**Test Coverage Needed**: Plugin handshake tests, process cleanup tests

---

### [ ] 9. Reorganize `crates/kn9t-server/src/` Module Hierarchy
**Complexity**: 🔥 Low-Medium  
**Pattern**: Too many root-level modules (11 at root)

**Current Bad Structure**:
```
├── state.rs
├── turn.rs
├── session.rs
├── policy.rs
├── tools.rs
├── lease.rs
├── host_api.rs
├── http_util.rs
├── config.rs
├── router.rs
├── routes/ ✓ (this is good)
```

**Target Structure**:
```
├── routes/ (keep as-is)
├── core/
│   ├── lib.rs (re-exports)
│   ├── auth.rs
│   ├── bus.rs
│   └── log.rs
├── config/ (extract from config.rs)
├── policy/ (extract from policy.rs)
├── session/
│   ├── lib.rs
│   ├── state.rs
│   ├── turn.rs
│   └── lease.rs
└── main.rs/http_util.rs (stay at root)
```

**Implementation**:
1. Create subdirectories
2. Move files incrementally
3. Update module declarations
4. Update all imports
5. Run full test suite

**Estimated Time**: 1 day  
**Risk**: Low (compilation errors catch issues)  
**Test Coverage Needed**: All route tests pass

---

## 🟢 MEDIUM PRIORITY (Do Soon After)

### [ ] 10. Reorganize `crates/kn9t-tui/src/` Module Hierarchy
**Complexity**: 🔥 Low-Medium  
**Pattern**: 28 modules at root level - navigation nightmare

**Target Structure**:
```
├── main.rs
├── lib.rs
├── lua/ (keep as-is)
├── ui/ (keep, maybe expand)
├── app/
│   ├── lib.rs (re-exports)
│   ├── core.rs
│   ├── events.rs
│   └── render.rs
├── state/
│   ├── lib.rs (re-exports)
│   ├── reducer.rs → reducer/
│   └── app.rs (move to app/)
├── render/ (if split from app)
│   ├── render.rs → ui/render/
│   ├── markdown.rs
│   ├── latex.rs
│   ├── diff_viewer.rs
│   └── theme.rs
├── widgets/
│   ├── lib.rs
│   ├── keybind.rs
│   ├── which_key.rs
│   ├── theme.rs
│   └── command_palette.rs
├── input/
│   ├── lib.rs
│   ├── input_history.rs
│   ├── slash.rs
│   └── prompt_history.rs
└── core/
    ├── lib.rs
    ├── client.rs
    ├── config.rs
    └── search.rs
```

**Notes**:
- This is purely organizational, no code changes
- Will dramatically improve navigability
- Easy to do after splitting large files

**Estimated Time**: 0.5-1 day  
**Risk**: Very Low (mechanical reorganization)  
**Test Coverage Needed**: Compilation verification only

---

### [ ] 11. Extract Policy Logic Modules
**Complexity**: 🔥 Low-Medium  
**File**: `crates/kn9t-server/src/policy.rs` (736 lines)

**Structure**:
```
crates/kn9t-server/src/policy/
├── mod.rs              (PolicyEngine, main logic)
├── match.rs            (pattern matching logic)
├── effect.rs           (effect application)
└── eval.rs             (decision evaluation)
```

**Steps**:
1. Extract pattern matching logic
2. Extract effect application
3. Keep evaluation in main
4. Add unit tests to each module

**Estimated Time**: 1 day  
**Risk**: Low (well-defined boundaries)  
**Test Coverage Needed**: Policy decision tests

---

### [ ] 12. Extract Server State Modules
**Complexity**: 🔥 Low-Medium  
**File**: `crates/kn9t-server/src/state.rs` (807 lines)

**Structure**:
```
crates/kn9t-server/src/session/
├── state.rs            (SessionState, main)
├── turn.rs             (TurnState, extracted)
├── tools.rs            (ToolCallState, extracted)
└── approval.rs         (ApprovalState, extracted)
```

**Steps**:
1. Extract turn state management
2. Extract tool call tracking
3. Extract approval handling
4. Keep session state in main
5. Define clear boundaries between sub-states

**Estimated Time**: 1 day  
**Risk**: Low (state management is isolatable)  
**Test Coverage Needed**: Session state transition tests

---

## 📋 VERIFICATION CHECKS (Before/After)

### Quality Metrics
- [ ] `cargo clippy --all` passes (0 warnings)
- [ ] `cargo test --all` passes (100% green)
- [ ] `cargo doc --no-deps` builds without errors
- [ ] No dead code warnings from `cargo dead-code` (if available)

### File Size Metrics
```
# Before
app.rs: 4189 lines
render.rs: 4108 lines
reducer.rs: 1559 lines
config.rs: 1231 lines
host.rs: 1183 lines
...

# After (targets)
app modules: avg 600 lines each (< 4189 total via split)
render modules: avg 700 lines each (< 4108 total via split)
reducer modules: avg 250 lines each
config modules: avg 300 lines each
host modules: avg 300 lines each
```

### Module Organization Checks
- [ ] No module exceeds 1500 lines (except tests/examples)
- [ ] No directory exceeds 15 files (except routes, ui, lua which are special)
- [ ] Max nesting depth is 3 levels from crate root
- [ ] Each module has clear responsibility

### Code Quality Checks
```bash
# Run these after refactoring
cargo clippy --all -- -W clippy::all
cargo check --all
cargo test --all
cargo doc --no-deps --open
```

---

## 🚀 PHASED ROLLOUT PLAN

### Phase 1: Foundation (Days 1-2)
- [ ] Consolidate SSE protocol → new crate
- [ ] Consolidate HTTP client setup
- [ ] Remove unused stubs

**Verification**: 
```
$ cargo test --all
$ cargo clippy --all
```

---

### Phase 2: Medium Modules (Days 3-5)
- [ ] Split config.rs → config/ subdir
- [ ] Refactor reducer.rs → reducer/ subdir
- [ ] Split host.rs → host/ subdir
- [ ] Extract policy modules

**Verification**: 
```
$ cargo test --all
$ git diff --stat (should show file reorganization)
```

---

### Phase 3: Large Modules (Days 6-10)
- [ ] Split lua/mod.rs → lua/api/ + vm modules
- [ ] Reorganize server root → subdirectories
- [ ] Reorganize tui root → subdirectories

**Verification**: All tests pass, module count per dir reduced

---

### Phase 4: Giant Files (Days 11-18)
- [ ] Split app.rs → app/ submodules (3-4 days)
- [ ] Split render.rs → ui/render/ submodules (4-5 days)

**Verification**: 
- Compile checks pass
- All TUI tests pass
- Visual regression tests pass
- Performance benchmarks maintained

---

### Phase 5: Documentation & Polish (Days 19-20)
- [ ] Update module-level documentation
- [ ] Create module responsibility matrix
- [ ] Add architecture diagrams
- [ ] Update CONTRIBUTING.md

---

## 📊 EXPECTED OUTCOMES

### Code Metrics
| Metric | Current | Target | Change |
|--------|---------|--------|--------|
| Max file size | 4189 | <1200 | -71% |
| Avg file size | 355 | <300 | -15% |
| Files >1000 LOC | 12 | 2 | -83% |
| Root modules (server) | 11 | 5 | -55% |
| Root modules (tui) | 28 | 8 | -71% |

### Developer Experience
- ✅ Easier to find code (better module hierarchy)
- ✅ Faster compilation (smaller modules compile faster)
- ✅ Easier code review (smaller PRs)
- ✅ Better test isolation (modules have focused tests)
- ✅ Reduced cognitive load (less code per file)

### Risk Reduction
- ✅ Changes isolated to single modules
- ✅ Testing parallelizes better
- ✅ Easier to understand control flow
- ✅ Clearer dependency graphs

---

## ⚠️ ROLLBACK PROCEDURES

For each phase, maintain:
1. Feature branch per major module split
2. Run full test suite before merge
3. Squash-merge into main for clean history
4. Tag each major milestone

```bash
# If something breaks:
git revert <commit-hash>  # Safe revert
git reflog              # See all history
```

---

*Checklist generated from codebase analysis. Adjust priorities based on team velocity and project needs.*
