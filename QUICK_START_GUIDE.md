# Quick Start Guide - Codebase Cleanup

**TL;DR**: Comprehensive analysis complete. Start with Quick Wins. Choose your path below.

---

## 🚀 3 Paths to Success

### Path A: Manager / Tech Lead
**Goal**: Understand scope and plan timeline

**Steps** (15 minutes):
1. Read **README_REPORTS.md** (overview)
2. Scan **CODEBASE_ANALYSIS_REPORT.md** sections 1, 7, 10
3. Review **Summary Table** in section 7
4. Decision: Assign work based on priority tiers

**Output**: Phased 2-3 week plan with resource allocation

---

### Path B: Developer (Getting Work)
**Goal**: Execute assigned cleanup task

**Steps** (varies by task):
1. Find your task in **CLEANUP_CHECKLIST.md**
2. Read the full section for your task (structure, steps, timeline)
3. Reference **DUPLICATION_PATTERNS_DETAIL.md** if needed (technical details)
4. Follow the step-by-step instructions
5. Run verification commands before submitting PR

**Output**: Completed refactoring with tests passing

---

### Path C: Architect / Code Reviewer
**Goal**: Understand technical decisions and validate quality

**Steps**:
1. Read **DUPLICATION_PATTERNS_DETAIL.md** for all patterns
2. Review **CLEANUP_CHECKLIST.md** verification sections
3. Check **CODEBASE_ANALYSIS_REPORT.md** architectural sections
4. Review PRs against checklist requirements

**Output**: Validation framework and quality gates

---

## 📍 Find Your Task

### Quick Wins (1 day - Start Here!)
```
[ ] Consolidate SSE → new crate (4 hours)
    → CLEANUP_CHECKLIST.md § 3
    → DUPLICATION_PATTERNS_DETAIL.md § 2

[ ] Consolidate HTTP client setup (1 day)
    → CLEANUP_CHECKLIST.md § 7
    → DUPLICATION_PATTERNS_DETAIL.md § 3

[ ] Remove unused stubs (30 mins)
    → CLEANUP_CHECKLIST.md § 5 (optional items)
    → DUPLICATION_PATTERNS_DETAIL.md § 5
```

### Medium Effort (3-5 days)
```
[ ] Split config.rs → config/ subdir
    → CLEANUP_CHECKLIST.md § 6

[ ] Refactor reducer.rs → reducer/ subdir
    → CLEANUP_CHECKLIST.md § 5

[ ] Split host.rs → host/ subdir
    → CLEANUP_CHECKLIST.md § 8

[ ] Reorganize server root → subdirs
    → CLEANUP_CHECKLIST.md § 9

[ ] Reorganize tui root → subdirs
    → CLEANUP_CHECKLIST.md § 10
```

### High Effort (5-7 days each)
```
[ ] Split lua/mod.rs → lua/api/
    → CLEANUP_CHECKLIST.md § 4
    → 2-3 days estimate

[ ] Split app.rs → app/ submodules
    → CLEANUP_CHECKLIST.md § 1
    → 3-4 days estimate

[ ] Split render.rs → ui/render/ submodules
    → CLEANUP_CHECKLIST.md § 2
    → 4-5 days estimate
```

---

## ⏱️ Timeline Options

### Aggressive (2 weeks, all hands)
```
Week 1:
  Mon-Tue: Quick Wins + SSE consolidation
  Wed-Fri: Start split config + host
  
Week 2:
  Mon-Wed: Complete medium effort items
  Thu-Fri: Start high effort items (lua)
```

### Balanced (3 weeks, incremental)
```
Week 1: Quick Wins only (1 day work, iterate)
Week 2: Medium effort items (SSE, config, reducer)
Week 3: High effort items (lua, app) in parallel tracks
```

### Conservative (4+ weeks, minimal disruption)
```
Do 1 quick win per week
Do 1 medium item per week
Plan high effort items for future sprint
```

---

## 🎯 Document Map

| Document | Size | Best For | Read Time |
|----------|------|----------|-----------|
| README_REPORTS.md | 11 KB | Overview, decision making | 5 min |
| CODEBASE_ANALYSIS_REPORT.md | 16 KB | Exec summary, metrics, roadmap | 15 min |
| CLEANUP_CHECKLIST.md | 15 KB | Step-by-step tasks | 20 min (per task) |
| DUPLICATION_PATTERNS_DETAIL.md | 16 KB | Technical deep dives | 30 min (per section) |
| This file | 3 KB | Quick navigation | 2 min |

---

## 💼 For Project Managers

**Timeline Estimate**: 2-3 weeks full cleanup  
**Resource**: 1-2 senior developers per week  
**Risk**: Low (mechanical refactoring, good test coverage)  
**Benefit**: 15-20% code reduction, massive DX improvement  

**Phased Approach**:
```
Week 1: 1 developer on Quick Wins (low risk, high visibility)
Week 2: 2 developers on medium effort (parallel tracks)
Week 3: 2-3 developers on giant files (complex, needs care)
```

**Success Criteria**:
- ✅ cargo clippy --all returns 0 warnings
- ✅ cargo test --all passes 100%
- ✅ No file exceeds 1500 lines
- ✅ No directory has 15+ root modules

---

## 🔧 For Developers

**Before Starting**:
```bash
# Ensure you're on clean main
git checkout main
git pull origin main

# Create feature branch
git checkout -b cleanup/my-task-name

# Verify build works
cargo check --all
cargo test --all
```

**During Work**:
```bash
# Follow checklist step-by-step
# Compile frequently
cargo check

# Run tests often
cargo test --lib --all

# Check your changes didn't break anything
cargo clippy --package my-modified-crate
```

**Before PR**:
```bash
# Full validation
cargo check --all
cargo test --all
cargo clippy --all -- -W clippy::all
cargo fmt --all

# Commit and push
git push origin cleanup/my-task-name
```

---

## 🧪 Verification Commands

### Quick Check (2 seconds)
```bash
cargo check --all
```

### Full Validation (2 minutes)
```bash
cargo test --all
cargo clippy --all
```

### Deep Validation (5 minutes)
```bash
cargo check --all
cargo test --all --verbose
cargo clippy --all -- -W clippy::all
cargo fmt --check
cargo doc --no-deps
```

---

## 📊 Metrics Snapshot

### Current State
```
Largest file:        app.rs (4,189 lines)
2nd largest:         render.rs (4,108 lines)
3rd largest:         reducer.rs (1,559 lines)

Total LOC:           ~65,000
Files >1000 LOC:     12
Avg file size:       355 lines
Root modules (tui):  28
Root modules (server): 11
```

### Target State
```
Largest file:        <1,200 lines (split)
2nd largest:         <1,200 lines (split)
Files >1000 LOC:     ~2 (tests only)

Total LOC:           ~58,000 (-6,500)
Avg file size:       <300 lines
Root modules (tui):  <10
Root modules (server): <6
```

---

## 🚨 Common Issues & Fixes

### Issue: "Compilation errors after refactoring"
```
Solution: 
1. Check all imports still exist
2. Verify mod.rs exports are correct
3. Use cargo check --all to find all errors
4. Fix compiler errors one at a time
```

### Issue: "Tests failing after split"
```
Solution:
1. Ensure test module is still in right place
2. Update use statements in test files
3. Run cargo test --lib to verify
4. Run integration tests separately
```

### Issue: "Performance regressed"
```
Solution:
1. Run cargo bench before/after split
2. Check if inlining hints are needed
3. Profile with perf/flamegraph tools
4. Consider keeping performance-critical code in fewer modules
```

### Issue: "Can't find where to add new code"
```
Solution:
1. Read the module responsibility list
2. Check the file that seems most appropriate
3. If no good fit, create focused new module
4. Document responsibility in module docs
```

---

## 📚 Learn More

### About This Project
- Read **DESIGN.md** for architecture decisions
- Read **AGENTS.md** for system capabilities
- Read **API.md** for public API

### About Refactoring
- Martin Fowler's "Refactoring" (classic book)
- "Clean Code" by Robert C. Martin
- Rust API Guidelines: api.rust-lang.org

### About Rust Module Organization
- https://doc.rust-lang.org/book/ch07-00-managing-growing-projects-with-packages-modules-and-paths.html
- https://doc.rust-lang.org/cargo/guide/
- This project's own module structure (once refactored!)

---

## 🎓 Decision Tree

```
START: I want to help with cleanup
│
├─ I'm a manager/lead
│  └─ Read: README_REPORTS.md (5 min)
│     Then: CODEBASE_ANALYSIS_REPORT.md § 7-10 (15 min)
│     Action: Assign tasks from CLEANUP_CHECKLIST.md
│
├─ I want to do Quick Wins
│  └─ Read: CLEANUP_CHECKLIST.md § 3, 7
│     Time: 4-8 hours
│     Difficulty: Low
│
├─ I want to do Medium work
│  └─ Read: CLEANUP_CHECKLIST.md § 4-10
│     Time: 1-3 days each
│     Difficulty: Medium
│
└─ I want to do Complex work
   └─ Read: CLEANUP_CHECKLIST.md § 1-2
      Also: DUPLICATION_PATTERNS_DETAIL.md § relevant section
      Time: 3-5 days each
      Difficulty: High
      Risk: Medium
```

---

## 🏁 Success Checklist

After completing each task:

- [ ] Code compiles without warnings (`cargo check`)
- [ ] All tests pass (`cargo test`)
- [ ] Clippy is happy (`cargo clippy`)
- [ ] Code is formatted (`cargo fmt`)
- [ ] New files have module docs
- [ ] PR references this analysis
- [ ] Review checklist requirements met
- [ ] Changes don't break performance (check benchmarks)

---

## 🔗 Quick Links

| What | Where |
|------|-------|
| Find my task | CLEANUP_CHECKLIST.md |
| Understand why | CODEBASE_ANALYSIS_REPORT.md |
| Technical details | DUPLICATION_PATTERNS_DETAIL.md |
| Timeline planning | CODEBASE_ANALYSIS_REPORT.md § 10 |
| Verification steps | CLEANUP_CHECKLIST.md (end of each item) |
| Code examples | DUPLICATION_PATTERNS_DETAIL.md |

---

## 💬 Questions?

| Question | Answer Location |
|----------|-----------------|
| "What should I work on?" | CLEANUP_CHECKLIST.md (pick by priority/difficulty) |
| "How long will it take?" | CLEANUP_CHECKLIST.md (time estimates on each item) |
| "What are the risks?" | CLEANUP_CHECKLIST.md § "Risk" for each item |
| "What exactly do I change?" | CLEANUP_CHECKLIST.md § "Steps" |
| "How do I verify my work?" | CLEANUP_CHECKLIST.md § "Verification" |
| "Why are we doing this?" | CODEBASE_ANALYSIS_REPORT.md § 1-4 |
| "How do we measure success?" | CODEBASE_ANALYSIS_REPORT.md § 7 |

---

**Ready?** Pick a task from CLEANUP_CHECKLIST.md and get started! 🚀

**Questions?** Check the document map above.

**Need planning help?** See "Timeline Options" or "For Project Managers" sections.
