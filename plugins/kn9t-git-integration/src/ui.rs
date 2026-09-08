//! Serializes git state to JSON and defines the interactive Lua view sent once
//! via `ui_register_lua`.
//!
//! Unlike the first version of this plugin, the Lua here is not render-only: it
//! binds its own keys and clicks via `kn9t.on_key`/`kn9t.on_click`, so the diff
//! review panel is genuinely owned by this plugin. A user does not have to add
//! anything to `tui.lua` beyond choosing where the view goes:
//!
//! ```lua
//! { type = "plugin", plugin = "kn9t-git-integration" }
//! ```

use crate::diff::{DiffFile, DiffTarget};
use crate::git::GitState;

/// JSON pushed via `ui_set_state`.
///
/// `repo` is `null` (not an empty object) when `cwd` is not a git repository,
/// so the Lua side can render a distinct message instead of an
/// empty-but-misleadingly-"clean" status.
use std::collections::HashMap;

/// Commit diffs: sha -> list of changed files with hunks
pub type CommitDiffs = HashMap<String, Vec<DiffFile>>;

pub fn state_to_json(
    state: Option<&GitState>,
    files: &[DiffFile],
    diff_target: &DiffTarget,
    commit_diffs: &CommitDiffs,
) -> serde_json::Value {
    let repo = match state {
        None => serde_json::Value::Null,
        Some(s) => serde_json::json!({
            "branch": s.branch,
            "ahead": s.ahead,
            "behind": s.behind,
            "changes": s.changes.iter().map(|c| serde_json::json!({
                "status": c.status,
                "path": c.path,
            })).collect::<Vec<_>>(),
            "recent": s.recent.iter().map(|l| serde_json::json!({
                "sha": l.sha,
                "subject": l.subject,
                "author": l.author,
                "date": l.date,
                "refs": l.refs,
                "graph": l.graph,
            })).collect::<Vec<_>>(),
            "refs": s.refs.iter().map(|r| serde_json::json!({
                "name": r.name,
                "kind": r.kind.tag(),
                "is_current": r.is_current,
            })).collect::<Vec<_>>(),
            "stashes": s.stashes,
        }),
    };

    serde_json::json!({
        "repo": repo,
        "diff": files.iter().map(diff_file_to_json).collect::<Vec<_>>(),
        "diff_target": diff_target.label(),
        "commit_diffs": commit_diffs.iter().map(|(sha, cfiles)| {
            serde_json::json!({
                "sha": sha,
                "files": cfiles.iter().map(diff_file_to_json).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
    })
}

fn diff_file_to_json(f: &DiffFile) -> serde_json::Value {
    serde_json::json!({
        "path": f.path,
        "status": f.status.tag(),
        "additions": f.additions,
        "deletions": f.deletions,
        "hunks": f.hunks.iter().map(|h| serde_json::json!({
            "header": h.header,
            "old_start": h.old_start,
            "new_start": h.new_start,
            "lines": h.lines.iter().map(|l| serde_json::json!({
                "kind": l.kind.tag(),
                "text": l.text,
                // Serialized per line rather than derived Lua-side from the
                // hunk start plus an index: removed lines occupy no new-file
                // line, so index arithmetic drifts after the first deletion.
                "new_lineno": l.new_lineno,
                "old_lineno": l.old_lineno,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// The plugin's Lua view: `render(state)` plus its own key and click bindings.
///
/// Sent once via `ui_register_lua` (256 KB cap — this is well under it).
///
/// # Why the state split
///
/// `state` (pushed from Rust) is data about the repository. `V` (Lua-local) is
/// view state: cursor, mode, comments. They are deliberately separate — a poll
/// arriving every few seconds must not reset the cursor the user just moved, so
/// nothing in `V` is ever derived from a fresh `state` except by clamping.
pub const LUA_SOURCE: &str = r##"
-- View state. Survives `ui_set_state`, which only replaces repo data.
V = {
  mode = "status",   -- "status" | "diff" | "graph" | "commit"
  split = false,     -- side-by-side vs unified, diff mode only
  tree = true,       -- show the file list
  file = 1,          -- 1-based index into state.diff
  cursor = 1,        -- 1-based index into the flattened line list
  scroll = 0,
  comments = {},     -- { {path=, line=, text=} }
  typing = nil,      -- in-progress comment text, nil when not composing
  height = 20,       -- last known viewport height, for paging
  -- Git graph filters
  show_local = true,
  show_remote = true,
  show_tags = false,
  show_stash = false,
  -- Graph navigation & pagination
  graph_cursor = 1,
  graph_scroll = 0,
  graph_page = 1,        -- current page (1-indexed)
  graph_page_size = 100, -- commits per page
  graph_search = nil,    -- search query string
  graph_searching = false, -- true when typing search query
  -- Refs panel
  refs_cursor = 1,
  show_refs = false,
  -- Commit view (when viewing a specific commit's diff)
  commit_sha = nil,    -- sha of commit being viewed
  commit_file = 1,     -- file index in commit diff
  commit_cursor = 1,   -- line cursor in commit diff
  commit_scroll = 0,
}

local LAST = nil  -- most recent state, so handlers can see repo data

local function files()
  if LAST == nil or LAST.diff == nil then return {} end
  return LAST.diff
end

-- Get commits from recent log (filtering out graph-only lines)
local function commits()
  if LAST == nil or LAST.repo == nil or LAST.repo.recent == nil then return {} end
  local out = {}
  for _, l in ipairs(LAST.repo.recent) do
    if l.sha and l.sha ~= "" then
      table.insert(out, l)
    end
  end
  return out
end

-- Get all commits (unfiltered, for total count)
local function all_commits()
  return commits()
end

-- Get filtered commits (by search query and ref filters)
local function filtered_commits()
  local all = commits()
  local out = {}
  
  for _, c in ipairs(all) do
    -- Apply search filter
    if V.graph_search and V.graph_search ~= "" then
      local query = string.lower(V.graph_search)
      local match = string.find(string.lower(c.sha or ""), query, 1, true)
        or string.find(string.lower(c.subject or ""), query, 1, true)
        or string.find(string.lower(c.author or ""), query, 1, true)
      if not match then
        goto continue
      end
    end
    
    -- Apply ref filters (only hide commits that ONLY have filtered-out refs)
    local has_visible_ref = false
    if not c.refs or #c.refs == 0 then
      -- No refs - always visible
      has_visible_ref = true
    else
      for _, ref in ipairs(c.refs) do
        local is_local = not string.find(ref, "origin/")
        local is_remote = string.find(ref, "origin/") ~= nil
        local is_tag = string.find(ref, "tag:") ~= nil
        
        if is_local and V.show_local then has_visible_ref = true end
        if is_remote and V.show_remote then has_visible_ref = true end
        if is_tag and V.show_tags then has_visible_ref = true end
        -- stashes are rare in refs, skip for now
      end
    end
    
    if has_visible_ref then
      table.insert(out, c)
    end
    
    ::continue::
  end
  return out
end

-- Get paginated commits for current page
local function paged_commits()
  local filtered = filtered_commits()
  local start_idx = (V.graph_page - 1) * V.graph_page_size + 1
  local end_idx = start_idx + V.graph_page_size - 1
  local out = {}
  for i = start_idx, math.min(end_idx, #filtered) do
    table.insert(out, filtered[i])
  end
  return out, #filtered
end

-- Total pages
local function total_pages()
  local filtered = filtered_commits()
  return math.max(1, math.ceil(#filtered / V.graph_page_size))
end

-- Get commit diff files (if viewing a specific commit)
local function commit_files()
  if LAST == nil or LAST.commit_diffs == nil or V.commit_sha == nil then return {} end
  for _, cd in ipairs(LAST.commit_diffs) do
    if cd.sha == V.commit_sha then
      return cd.files or {}
    end
  end
  return {}
end

local function cur_commit_file()
  local f = commit_files()
  if #f == 0 then return nil end
  if V.commit_file > #f then V.commit_file = #f end
  if V.commit_file < 1 then V.commit_file = 1 end
  return f[V.commit_file]
end

-- Clamp graph cursor
local function clamp_graph_cursor()
  local c = commits()
  if V.graph_cursor < 1 then V.graph_cursor = 1 end
  if #c > 0 and V.graph_cursor > #c then V.graph_cursor = #c end
  -- Scroll adjustment
  local view = math.max(1, V.height - 3)
  if V.graph_cursor <= V.graph_scroll then V.graph_scroll = V.graph_cursor - 1 end
  if V.graph_cursor > V.graph_scroll + view then V.graph_scroll = V.graph_cursor - view end
  if V.graph_scroll < 0 then V.graph_scroll = 0 end
end

local function cur_file()
  local f = files()
  if #f == 0 then return nil end
  if V.file > #f then V.file = #f end
  if V.file < 1 then V.file = 1 end
  return f[V.file]
end

-- Flatten a file's hunks into display rows. Hunk headers become rows too, so
-- `]`/`[` can navigate to them and the cursor index means one thing only.
local function rows(file)
  local out = {}
  if file == nil then return out end
  for _, h in ipairs(file.hunks or {}) do
    table.insert(out, { hunk = true, text = h.header })
    for _, l in ipairs(h.lines or {}) do
      table.insert(out, {
        kind = l.kind,
        text = l.text,
        new_lineno = l.new_lineno,
        old_lineno = l.old_lineno,
      })
    end
  end
  return out
end

-- Flatten a commit file's hunks into display rows (same as rows() but reusable)
local function commit_rows(file)
  return rows(file)  -- Same logic, just a different name for clarity
end

local function clamp_cursor(n)
  if V.cursor < 1 then V.cursor = 1 end
  if n > 0 and V.cursor > n then V.cursor = n end
  -- Keep the cursor inside the viewport, accounting for the header row.
  local view = math.max(1, V.height - 2)
  if V.cursor <= V.scroll then V.scroll = V.cursor - 1 end
  if V.cursor > V.scroll + view then V.scroll = V.cursor - view end
  if V.scroll < 0 then V.scroll = 0 end
end

-- Clamp commit cursor and scroll (separate from diff mode)
local function clamp_commit_cursor(n)
  if V.commit_cursor < 1 then V.commit_cursor = 1 end
  if n > 0 and V.commit_cursor > n then V.commit_cursor = n end
  local view = math.max(1, V.height - 2)
  if V.commit_cursor <= V.commit_scroll then V.commit_scroll = V.commit_cursor - 1 end
  if V.commit_cursor > V.commit_scroll + view then V.commit_scroll = V.commit_cursor - view end
  if V.commit_scroll < 0 then V.commit_scroll = 0 end
end

local function comment_at(path, line)
  for _, c in ipairs(V.comments) do
    if c.path == path and c.line == line then return c.text end
  end
  return nil
end

-- ── input ────────────────────────────────────────────────────────────────────

-- All bindings go through `bind`, which owns one rule: while a comment is being
-- composed, a single printable key is text, not a command. Registering the
-- semantic keys and then a separate printable loop would clobber them (`d`, `u`,
-- `c`, `j` are all both), so there is exactly one handler per key.
local BOUND = {}

local function bind(key, fn)
  BOUND[key] = true
  kn9t.on_key(key, function()
    -- Handle search input in graph mode
    if V.graph_searching and #key == 1 then
      V.graph_search = V.graph_search .. key
      V.graph_page = 1  -- Reset to first page on new search
      V.graph_cursor = 1
      V.graph_scroll = 0
      return true
    end
    -- Handle comment input
    if V.typing ~= nil and #key == 1 then
      V.typing = V.typing .. key
      return true
    end
    return fn()
  end)
end

bind("j", function()
  if V.mode == "graph" then
    V.graph_cursor = V.graph_cursor + 1
    clamp_graph_cursor()
  elseif V.mode == "commit" then
    V.commit_cursor = V.commit_cursor + 1
    clamp_commit_cursor(#commit_rows(cur_commit_file()))
  else
    V.cursor = V.cursor + 1
    clamp_cursor(#rows(cur_file()))
  end
end)
bind("k", function()
  if V.mode == "graph" then
    V.graph_cursor = V.graph_cursor - 1
    clamp_graph_cursor()
  elseif V.mode == "commit" then
    V.commit_cursor = V.commit_cursor - 1
    clamp_commit_cursor(#commit_rows(cur_commit_file()))
  else
    V.cursor = V.cursor - 1
    clamp_cursor(#rows(cur_file()))
  end
end)
bind("Down", function()
  if V.mode == "graph" then
    V.graph_cursor = V.graph_cursor + 1
    clamp_graph_cursor()
  elseif V.mode == "commit" then
    V.commit_cursor = V.commit_cursor + 1
    clamp_commit_cursor(#commit_rows(cur_commit_file()))
  else
    V.cursor = V.cursor + 1
    clamp_cursor(#rows(cur_file()))
  end
end)
bind("Up", function()
  if V.mode == "graph" then
    V.graph_cursor = V.graph_cursor - 1
    clamp_graph_cursor()
  elseif V.mode == "commit" then
    V.commit_cursor = V.commit_cursor - 1
    clamp_commit_cursor(#commit_rows(cur_commit_file()))
  else
    V.cursor = V.cursor - 1
    clamp_cursor(#rows(cur_file()))
  end
end)

bind("PageDown", function()
  local step = math.max(1, V.height - 2)
  if V.mode == "graph" then
    V.graph_cursor = V.graph_cursor + step
    clamp_graph_cursor()
  elseif V.mode == "commit" then
    V.commit_cursor = V.commit_cursor + step
    clamp_commit_cursor(#commit_rows(cur_commit_file()))
  else
    V.cursor = V.cursor + step
    clamp_cursor(#rows(cur_file()))
  end
end)
bind("PageUp", function()
  local step = math.max(1, V.height - 2)
  if V.mode == "graph" then
    V.graph_cursor = V.graph_cursor - step
    clamp_graph_cursor()
  elseif V.mode == "commit" then
    V.commit_cursor = V.commit_cursor - step
    clamp_commit_cursor(#commit_rows(cur_commit_file()))
  else
    V.cursor = V.cursor - step
    clamp_cursor(#rows(cur_file()))
  end
end)

bind("n", function()
  if V.mode == "commit" then
    local f = commit_files()
    if V.commit_file < #f then V.commit_file = V.commit_file + 1; V.commit_cursor = 1; V.commit_scroll = 0 end
  else
    local f = files()
    if V.file < #f then V.file = V.file + 1; V.cursor = 1; V.scroll = 0 end
  end
end)
bind("p", function()
  if V.mode == "commit" then
    if V.commit_file > 1 then V.commit_file = V.commit_file - 1; V.commit_cursor = 1; V.commit_scroll = 0 end
  else
    if V.file > 1 then V.file = V.file - 1; V.cursor = 1; V.scroll = 0 end
  end
end)

bind("u", function() V.split = not V.split end)
bind("b", function() V.tree = not V.tree end)
bind("d", function()
  if V.mode == "diff" then
    V.mode = "status"
  elseif V.mode == "commit" then
    V.mode = "graph"  -- back to graph from commit view
  else
    V.mode = "diff"
  end
  V.cursor = 1
  V.scroll = 0
end)

-- Toggle graph view
bind("g", function()
  if V.mode == "graph" then
    V.mode = "status"
  elseif V.mode == "commit" then
    V.mode = "graph"  -- back to graph from commit view
  else
    V.mode = "graph"
    V.graph_cursor = 1
    V.graph_scroll = 0
  end
end)

-- Enter: view commit diff in graph mode, or select in other modes
bind("Enter", function()
  if V.mode == "graph" then
    local c = commits()
    if #c > 0 and V.graph_cursor <= #c then
      local commit = c[V.graph_cursor]
      if commit and commit.sha and commit.sha ~= "" then
        V.commit_sha = commit.sha
        V.commit_file = 1
        V.commit_cursor = 1
        V.commit_scroll = 0
        V.mode = "commit"
      end
    end
    return true
  end
  return true
end)

-- Escape/Backspace: go back from commit view to graph
bind("Backspace", function()
  if V.typing ~= nil then
    V.typing = string.sub(V.typing, 1, -2)
    return true
  end
  if V.mode == "commit" then
    V.mode = "graph"
    return true
  end
  return true
end)

-- Toggle refs panel
bind("r", function()
  V.show_refs = not V.show_refs
end)

-- Filter toggles (in graph mode)
bind("1", function()
  if V.mode == "graph" or V.mode == "status" then
    V.show_local = not V.show_local
  end
end)
bind("2", function()
  if V.mode == "graph" or V.mode == "status" then
    V.show_remote = not V.show_remote
  end
end)
bind("3", function()
  if V.mode == "graph" or V.mode == "status" then
    V.show_tags = not V.show_tags
  end
end)
bind("4", function()
  if V.mode == "graph" or V.mode == "status" then
    V.show_stash = not V.show_stash
  end
end)

-- Pagination: [ prev page, ] next page
bind("[", function()
  if V.mode == "graph" then
    if V.graph_page > 1 then
      V.graph_page = V.graph_page - 1
      V.graph_cursor = 1
      V.graph_scroll = 0
    end
    return true
  end
  -- Original behavior for diff mode (prev hunk)
  local r = rows(cur_file())
  for i = V.cursor - 1, 1, -1 do
    if r[i].hunk then V.cursor = i; clamp_cursor(#r); return true end
  end
  if V.file > 1 then
    V.file = V.file - 1
    local pr = rows(cur_file())
    V.cursor = #pr > 0 and #pr or 1
    clamp_cursor(#pr)
  end
  return true
end)

bind("]", function()
  if V.mode == "graph" then
    if V.graph_page < total_pages() then
      V.graph_page = V.graph_page + 1
      V.graph_cursor = 1
      V.graph_scroll = 0
    end
    return true
  end
  -- Original behavior for diff mode (next hunk)
  local r = rows(cur_file())
  for i = V.cursor + 1, #r do
    if r[i].hunk then V.cursor = i; clamp_cursor(#r); return true end
  end
  local f = files()
  if V.file < #f then V.file = V.file + 1; V.cursor = 1; V.scroll = 0 end
  return true
end)

-- Search: Ctrl+F to start search in graph mode
bind("C-f", function()
  if V.mode == "graph" then
    V.graph_searching = true
    V.graph_search = ""
    return true
  end
  return true
end)

-- Comment capture. `c` opens composition; printable keys then accumulate via
-- the `bind` wrapper; Enter commits. Esc is deliberately unbound: the host uses
-- it to blur the panel, and trapping it would leave no way out.
bind("c", function()
  if V.mode == "diff" or V.mode == "commit" then
    V.typing = ""
  end
  return true
end)

-- Enter and Backspace are not single printable keys, so they reach here even
-- while composing and mean "commit" / "erase" rather than text.
bind("Enter", function()
  -- Confirm search
  if V.graph_searching then
    V.graph_searching = false
    return true
  end
  
  -- Handle comment submission in diff mode
  if V.typing ~= nil and V.mode == "diff" then
    local f = cur_file()
    local r = rows(f)
    local row = r[V.cursor]
    if f ~= nil and row ~= nil and V.typing ~= "" then
      local line = row.new_lineno or row.old_lineno
      if line ~= nil then
        table.insert(V.comments, { path = f.path, line = line, text = V.typing })
      end
    end
    V.typing = nil
    return true
  end
  
  -- Handle comment submission in commit mode
  if V.typing ~= nil and V.mode == "commit" then
    local f = cur_commit_file()
    local r = commit_rows(f)
    local row = r[V.commit_cursor]
    if f ~= nil and row ~= nil and V.typing ~= "" then
      local line = row.new_lineno or row.old_lineno
      if line ~= nil then
        -- Format: commit:sha file:path line:N : comment
        table.insert(V.comments, { 
          commit = V.commit_sha,
          path = f.path, 
          line = line, 
          text = V.typing 
        })
      end
    end
    V.typing = nil
    return true
  end
  
  -- Handle Enter in graph mode to view commit
  if V.mode == "graph" then
    local paged, _ = paged_commits()
    if #paged > 0 and V.graph_cursor <= #paged then
      local commit = paged[V.graph_cursor]
      if commit and commit.sha and commit.sha ~= "" then
        V.commit_sha = commit.sha
        V.commit_file = 1
        V.commit_cursor = 1
        V.commit_scroll = 0
        V.mode = "commit"
        -- Signal Rust to load diff via tmp file (kn9t.write_file provided by host)
        local tmp_dir = os.getenv("TEMP") or os.getenv("TMP") or "/tmp"
        kn9t.write_file(tmp_dir .. "/kn9t-commit-sha", commit.sha)
      end
    end
    return true
  end
  
  return true
end)

bind("Backspace", function()
  -- Handle search backspace
  if V.graph_searching then
    V.graph_search = string.sub(V.graph_search or "", 1, -2)
    V.graph_page = 1
    V.graph_cursor = 1
    V.graph_scroll = 0
    return true
  end
  -- Handle comment backspace
  if V.typing ~= nil then
    V.typing = string.sub(V.typing, 1, -2)
    return true
  end
  if V.mode == "commit" then
    V.mode = "graph"
    V.commit_sha = nil
    -- Clear the diff request
    local tmp_dir = os.getenv("TEMP") or os.getenv("TMP") or "/tmp"
    kn9t.write_file(tmp_dir .. "/kn9t-commit-sha", "")
    return true
  end
  return true
end)

-- Esc cancels search or comment composition; otherwise let host handle it (unfocus)
kn9t.on_key("Escape", function()
  if V.graph_searching then
    V.graph_searching = false
    V.graph_search = nil
    V.graph_page = 1
    V.graph_cursor = 1
    V.graph_scroll = 0
    return true  -- Cancel search, stay focused
  end
  if V.typing ~= nil then
    V.typing = nil
    return true  -- Consume: cancel comment, stay focused
  end
  return false  -- Let host unfocus the panel
end)

-- Hand the collected review to the prompt. This is the one host mutation a
-- plugin view can request, and the workflow the whole panel exists for.
bind("C-s", function()
  if #V.comments == 0 then return false end
  local parts = {}
  for _, c in ipairs(V.comments) do
    if c.commit then
      -- Commit comment format: [commit:sha file:path line:N] comment
      table.insert(parts, "[commit:" .. c.commit .. " " .. c.path .. ":" .. c.line .. "] " .. c.text)
    else
      -- Working tree comment format: [path:line] comment
      table.insert(parts, "[" .. c.path .. ":" .. c.line .. "] " .. c.text)
    end
  end
  kn9t.insert_input(table.concat(parts, "\n"))
  V.comments = {}
end)

-- Remaining printable characters: always consume them when focused to prevent
-- typing in the user input. When composing a comment, append to V.typing.
local PRINTABLE = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.,:;!?/()<>-_=+*#@'\"`~$%^&|\\"
for i = 1, #PRINTABLE do
  local ch = string.sub(PRINTABLE, i, i)
  if not BOUND[ch] then
    bind(ch, function() return true end)  -- Always consume when focused
  end
end

-- Space is sent as "Space" by the TUI, not " ". Always consume.
kn9t.on_key("Space", function()
  if V.typing ~= nil then
    V.typing = V.typing .. " "
  end
  return true  -- Always consume
end)

-- Clicking a file row selects it; clicking a diff row moves the cursor there.
kn9t.on_click("files", function(x, y)
  local f = files()
  local idx = y + 1
  if idx >= 1 and idx <= #f then
    V.file = idx
    V.cursor = 1
    V.scroll = 0
  end
end)

-- Clicking a graph row selects/enters the commit
kn9t.on_click("graph", function(x, y)
  if V.mode ~= "graph" then return false end
  local c = commits()
  local idx = y + 1
  if idx >= 1 and idx <= #c then
    V.graph_cursor = idx
    local commit = c[idx]
    if commit and commit.sha and commit.sha ~= "" then
      V.commit_sha = commit.sha
      V.commit_file = 1
      V.commit_cursor = 1
      V.commit_scroll = 0
      V.mode = "commit"
    end
  end
end)

kn9t.on_click("body", function(x, y)
  if V.mode ~= "diff" then return false end
  local target = V.scroll + y + 1
  local n = #rows(cur_file())
  if target >= 1 and target <= n then
    if target == V.cursor and V.typing == nil then
      -- Double-click on same line: start commenting
      V.typing = ""
    else
      V.cursor = target
      clamp_cursor(n)
    end
  end
end)

-- ── render ───────────────────────────────────────────────────────────────────

local function line_style(kind)
  if kind == "add" then return "green" end
  if kind == "del" then return "lightred" end
  return "gray"
end

local function line_bg(kind)
  if kind == "add" then return "#1a2e1a" end
  if kind == "del" then return "#2e1a1a" end
  return nil
end

-- Graph character styling
local function graph_char_color(ch)
  if ch == "*" then return "yellow" end
  if ch == "|" then return "blue" end
  if ch == "/" or ch == "\\" then return "magenta" end
  return "darkgray"
end

-- Render colored graph prefix
local function graph_spans(graph_str)
  local spans = {}
  for i = 1, #graph_str do
    local ch = string.sub(graph_str, i, i)
    table.insert(spans, { text = ch, fg = graph_char_color(ch) })
  end
  return spans
end

-- Ref badge color
local function ref_color(ref_str)
  if string.find(ref_str, "HEAD") then return "yellow" end
  if string.find(ref_str, "origin/") then return "lightred" end
  if string.find(ref_str, "tag:") then return "cyan" end
  return "green"
end

-- Refs sidebar
local function refs_panel(repo)
  local items = {}
  for i, r in ipairs(repo.refs or {}) do
    local dominated = false
    if r.kind == "local" and not V.show_local then dominated = true end
    if r.kind == "remote" and not V.show_remote then dominated = true end
    if r.kind == "tag" and not V.show_tags then dominated = true end
    if not dominated then
      local col = "gray"
      if r.kind == "local" then col = r.is_current and "green" or "white" end
      if r.kind == "remote" then col = "lightred" end
      if r.kind == "tag" then col = "cyan" end
      table.insert(items, { spans = {
        { text = "  ", fg = col },
        { text = r.name, fg = col, bold = r.is_current },
      }})
    end
  end
  if V.show_stash then
    for _, s in ipairs(repo.stashes or {}) do
      table.insert(items, { spans = {
        { text = "  ", fg = "magenta" },
        { text = s, fg = "magenta" },
      }})
    end
  end
  return {
    type = "list",
    id = "refs",
    items = items,
    size = { fixed = 28 },
  }
end

local function status_view(repo)
  local out = {}
  local branch = repo.branch or "?"
  local ab = ""
  if (repo.ahead or 0) > 0 then ab = ab .. " ↑" .. repo.ahead end
  if (repo.behind or 0) > 0 then ab = ab .. " ↓" .. repo.behind end
  table.insert(out, { type = "text", content = " " .. branch .. ab, fg = "cyan", bold = true,
                      size = { fixed = 1 }, wrap = false })

  local changes = repo.changes or {}
  if #changes == 0 then
    table.insert(out, { type = "text", content = "  (clean)", fg = "darkgray",
                        size = { fixed = 1 }, wrap = false })
  else
    table.insert(out, { type = "text", content = string.format("  %d change(s)", #changes), 
                        fg = "yellow", size = { fixed = 1 }, wrap = false })
    for _, c in ipairs(changes) do
      local col = "yellow"
      if c.status == "?" then col = "green"
      elseif c.status == "D" then col = "lightred"
      elseif c.status == "A" then col = "green" end
      table.insert(out, { type = "text", content = "  " .. c.status .. " " .. c.path, fg = col,
                          size = { fixed = 1 }, wrap = false })
    end
  end

  table.insert(out, { type = "text", content = "", size = { fixed = 1 } })
  
  -- Filter status bar
  local filter_spans = {
    { text = " Filters: ", fg = "darkgray" },
    { text = "[1]", fg = "cyan" },
    { text = V.show_local and "Local " or "local ", fg = V.show_local and "green" or "darkgray" },
    { text = "[2]", fg = "cyan" },
    { text = V.show_remote and "Remote " or "remote ", fg = V.show_remote and "lightred" or "darkgray" },
    { text = "[3]", fg = "cyan" },
    { text = V.show_tags and "Tags " or "tags ", fg = V.show_tags and "yellow" or "darkgray" },
    { text = "[4]", fg = "cyan" },
    { text = V.show_stash and "Stash" or "stash", fg = V.show_stash and "magenta" or "darkgray" },
  }
  table.insert(out, { type = "text", spans = filter_spans, size = { fixed = 1 }, wrap = false })
  
  table.insert(out, { type = "text", content = "", size = { fixed = 1 } })
  table.insert(out, { type = "text", content = " Recent commits:", fg = "darkgray",
                      size = { fixed = 1 }, wrap = false })
  
  -- Show graph preview (recent commits with graph)
  for i, l in ipairs(repo.recent or {}) do
    if i > 8 then break end
    if l.sha == "" then
      -- Graph-only line (merge connector)
      table.insert(out, { type = "text", spans = graph_spans("  " .. l.graph), 
                          size = { fixed = 1 }, wrap = false })
    else
      local spans = { { text = "  ", fg = "darkgray" } }
      for _, s in ipairs(graph_spans(l.graph)) do table.insert(spans, s) end
      table.insert(spans, { text = l.sha .. " ", fg = "yellow" })
      table.insert(spans, { text = l.subject, fg = "white" })
      for _, ref in ipairs(l.refs or {}) do
        if ref ~= "" then
          table.insert(spans, { text = " (" .. ref .. ")", fg = ref_color(ref) })
        end
      end
      table.insert(out, { type = "text", spans = spans, size = { fixed = 1 }, wrap = false })
    end
  end

  -- Help bar at bottom
  table.insert(out, { type = "spacer", size = { flex = 1 } })
  table.insert(out, { type = "text", spans = {
    { text = "[d]", fg = "cyan" }, { text = " diff  ", fg = "darkgray" },
    { text = "[g]", fg = "cyan" }, { text = " graph  ", fg = "darkgray" },
    { text = "[r]", fg = "cyan" }, { text = " refs  ", fg = "darkgray" },
    { text = "[Esc]", fg = "cyan" }, { text = " close", fg = "darkgray" },
  }, size = { fixed = 1 }, wrap = false })

  if V.show_refs then
    return {
      type = "split", direction = "horizontal",
      children = {
        refs_panel(repo),
        { type = "split", direction = "vertical", children = out },
      },
    }
  end
  return { type = "split", direction = "vertical", children = out }
end

-- Full graph view with pagination and search
local function graph_view(repo)
  local items = {}
  local paged, total_count = paged_commits()
  local pages = total_pages()
  
  for i, l in ipairs(paged) do
    local mark = (i == V.graph_cursor) and ">" or " "
    
    if l.sha == "" then
      local spans = { { text = mark, fg = "yellow" } }
      for _, s in ipairs(graph_spans(l.graph)) do table.insert(spans, s) end
      table.insert(items, { spans = spans })
    else
      local spans = { { text = mark, fg = "yellow", bold = i == V.graph_cursor } }
      for _, s in ipairs(graph_spans(l.graph)) do table.insert(spans, s) end
      table.insert(spans, { text = l.sha, fg = "yellow", bold = true })
      table.insert(spans, { text = " " })
      for _, ref in ipairs(l.refs or {}) do
        if ref ~= "" then
          table.insert(spans, { text = "[" .. ref .. "] ", fg = ref_color(ref) })
        end
      end
      table.insert(spans, { text = l.subject, fg = "white" })
      if l.author ~= "" then
        table.insert(spans, { text = " - " .. l.author .. ", " .. l.date, fg = "darkgray" })
      end
      table.insert(items, { spans = spans })
    end
  end
  
  -- List with current selection; TUI handles scrolling automatically
  local graph_list = { type = "list", id = "graph", items = items, selected = V.graph_cursor - 1, offset = V.graph_scroll }
  
  -- Header with search and pagination info
  local header_spans = {
    { text = " Git Graph  ", fg = "cyan", bold = true },
  }
  
  -- Show search query if searching
  if V.graph_searching then
    table.insert(header_spans, { text = "Search: ", fg = "yellow" })
    table.insert(header_spans, { text = V.graph_search .. "_", fg = "white", bold = true })
  elseif V.graph_search and V.graph_search ~= "" then
    table.insert(header_spans, { text = "\"" .. V.graph_search .. "\" ", fg = "yellow" })
    table.insert(header_spans, { text = "(" .. total_count .. " matches) ", fg = "darkgray" })
  end
  
  -- Pagination info
  if pages > 1 then
    table.insert(header_spans, { text = string.format(" Page %d/%d ", V.graph_page, pages), fg = "magenta" })
  end
  
  local footer_spans = {
    { text = "[j/k]", fg = "cyan" }, { text = " nav  ", fg = "darkgray" },
    { text = "[[/]]", fg = "cyan" }, { text = " page  ", fg = "darkgray" },
    { text = "[C-f]", fg = "cyan" }, { text = " search  ", fg = "darkgray" },
    { text = "[Enter]", fg = "cyan" }, { text = " view  ", fg = "darkgray" },
    { text = "[g]", fg = "cyan" }, { text = " close", fg = "darkgray" },
  }
  
  local children = {
    { type = "text", spans = header_spans, size = { fixed = 1 }, wrap = false },
    graph_list,
    { type = "text", spans = footer_spans, size = { fixed = 1 }, wrap = false },
  }
  
  if V.show_refs then
    return {
      type = "split", direction = "horizontal",
      children = {
        refs_panel(repo),
        { type = "split", direction = "vertical", children = children },
      },
    }
  end
  return { type = "split", direction = "vertical", children = children }
end

-- Comment at a specific line in a commit file
local function commit_comment_at(sha, path, line)
  for _, c in ipairs(V.comments) do
    if c.commit == sha and c.path == path and c.line == line then
      return c.text
    end
  end
  return nil
end

-- File list for commit view
local function commit_file_list()
  local items = {}
  local cdiff = commit_files()
  for i, f in ipairs(cdiff) do
    local col = "yellow"
    if f.status == "A" then col = "green"
    elseif f.status == "D" then col = "lightred" end
    local selected = (i == V.commit_file)
    table.insert(items, { spans = {
      { text = selected and ">" or " ", fg = "yellow" },
      { text = f.status .. " ", fg = col },
      { text = f.path .. " " },
      { text = "+" .. (f.additions or 0), fg = "green" },
      { text = " -" .. (f.deletions or 0), fg = "lightred" },
    }})
  end
  return {
    type = "list",
    id = "commit_files",
    items = items,
    selected = V.commit_file - 1,
    size = { fixed = 34 },
  }
end

-- Commit unified body: one row per line, with the cursor row marked
local function commit_unified_body(file)
  local r = commit_rows(file)
  local items = {}
  for i, row in ipairs(r) do
    if row.hunk then
      table.insert(items, { spans = { { text = row.text, fg = "cyan", bold = true } } })
    else
      local mark = (i == V.commit_cursor) and ">" or " "
      local num = row.new_lineno or row.old_lineno
      local prefix = (row.kind == "add" and "+") or (row.kind == "del" and "-") or " "
      local bg = line_bg(row.kind)
      table.insert(items, { spans = {
        { text = mark, fg = "yellow", bold = true, bg = bg },
        { text = string.format("%5s ", num and tostring(num) or ""), fg = "darkgray", bg = bg },
        { text = prefix, fg = line_style(row.kind), bg = bg },
        { text = row.text, syntax = file.path, bg = bg },
      }})
      local existing = commit_comment_at(V.commit_sha, file.path, num)
      if existing ~= nil then
        table.insert(items, { spans = { { text = "      > " .. existing, fg = "magenta" } } })
      end
      if i == V.commit_cursor and V.typing ~= nil then
        table.insert(items, { spans = { { text = "      > " .. V.typing .. "_", fg = "magenta" } } })
      end
    end
  end
  return { type = "list", id = "commit_body", items = items, offset = V.commit_scroll }
end

-- Commit detail view: show commit info and its diff
local function commit_view(repo)
  -- Find the selected commit
  local commit = nil
  for _, l in ipairs(repo.recent or {}) do
    if l.sha == V.commit_sha then
      commit = l
      break
    end
  end
  
  if commit == nil then
    return { type = "text", content = "Commit not found: " .. (V.commit_sha or "nil"), fg = "lightred" }
  end
  
  local cdiff = commit_files()
  
  -- If no diff available, show simple view
  if #cdiff == 0 then
    local out = {}
    table.insert(out, { type = "text", spans = {
      { text = " Commit ", fg = "cyan", bold = true },
      { text = commit.sha, fg = "yellow", bold = true },
      { text = " - " .. (commit.author or ""), fg = "darkgray" },
    }, size = { fixed = 1 }, wrap = false })
    table.insert(out, { type = "text", spans = {
      { text = " " .. commit.subject, fg = "white", bold = true },
    }, size = { fixed = 1 }, wrap = false })
    table.insert(out, { type = "text", content = " (loading diff...)", fg = "darkgray" })
    table.insert(out, { type = "spacer", size = { flex = 1 } })
    table.insert(out, { type = "text", spans = {
      { text = "[Backspace]", fg = "cyan" }, { text = " back", fg = "darkgray" },
    }, size = { fixed = 1 }, wrap = false })
    return { type = "split", direction = "vertical", children = out }
  end
  
  -- Full diff view like diff_view
  local f = cur_commit_file()
  local head = string.format(" %s  %s  %d/%d",
    commit.sha, f and f.path or "?", V.commit_file, #cdiff)
  
  local body = commit_unified_body(f)
  
  local children = {
    { type = "text", content = head, fg = "cyan", bold = true,
      size = { fixed = 1 }, wrap = false },
  }
  
  if V.tree then
    table.insert(children, {
      type = "split", direction = "horizontal",
      children = { commit_file_list(), body },
    })
  else
    table.insert(children, body)
  end
  
  -- Bottom bar: comment input or help
  if V.typing ~= nil then
    table.insert(children, {
      type = "text", content = "comment> " .. V.typing .. "  (Enter: save, Esc: cancel)",
      fg = "magenta", size = { fixed = 1 }, wrap = false,
    })
  else
    local spans = {}
    local commit_comments = 0
    for _, c in ipairs(V.comments) do
      if c.commit then commit_comments = commit_comments + 1 end
    end
    if commit_comments > 0 then
      table.insert(spans, { text = "[C-s]", fg = "cyan" })
      table.insert(spans, { text = string.format(" send %d  ", commit_comments), fg = "yellow" })
    end
    table.insert(spans, { text = "[j/k]", fg = "cyan" })
    table.insert(spans, { text = " nav  ", fg = "darkgray" })
    table.insert(spans, { text = "[n/p]", fg = "cyan" })
    table.insert(spans, { text = " file  ", fg = "darkgray" })
    table.insert(spans, { text = "[c]", fg = "cyan" })
    table.insert(spans, { text = " comment  ", fg = "darkgray" })
    table.insert(spans, { text = "[Backspace]", fg = "cyan" })
    table.insert(spans, { text = " back", fg = "darkgray" })
    table.insert(children, {
      type = "text", spans = spans,
      size = { fixed = 1 }, wrap = false,
    })
  end
  
  return { type = "split", direction = "vertical", children = children }
end

local function file_list()
  local items = {}
  for _, f in ipairs(files()) do
    local col = "yellow"
    if f.status == "A" then col = "green"
    elseif f.status == "D" then col = "lightred" end
    table.insert(items, { spans = {
      { text = f.status .. " ", fg = col },
      { text = f.path .. " " },
      { text = "+" .. f.additions, fg = "green" },
      { text = " -" .. f.deletions, fg = "lightred" },
    }})
  end
  return {
    type = "list",
    id = "files",
    items = items,
    selected = V.file - 1,
    size = { fixed = 34 },
  }
end

-- Unified body: one row per line, with the cursor row marked and any comment
-- shown inline beneath its anchor.
local function unified_body(file)
  local r = rows(file)
  local items = {}
  for i, row in ipairs(r) do
    if row.hunk then
      table.insert(items, { spans = { { text = row.text, fg = "cyan", bold = true } } })
    else
      local mark = (i == V.cursor) and ">" or " "
      local num = row.new_lineno or row.old_lineno
      local prefix = (row.kind == "add" and "+") or (row.kind == "del" and "-") or " "
      local bg = line_bg(row.kind)
      table.insert(items, { spans = {
        { text = mark, fg = "yellow", bold = true, bg = bg },
        { text = string.format("%5s ", num and tostring(num) or ""), fg = "darkgray", bg = bg },
        { text = prefix, fg = line_style(row.kind), bg = bg },
        { text = row.text, syntax = file.path, bg = bg },
      }})
      local existing = comment_at(file.path, num)
      if existing ~= nil then
        table.insert(items, { spans = { { text = "      > " .. existing, fg = "magenta" } } })
      end
    end
  end
  return { type = "list", id = "body", items = items, offset = V.scroll }
end

-- Side-by-side body. Removed lines occupy the left column, added the right,
-- context both — which is what makes a rename or a reflow readable.
local function split_body(file)
  local r = rows(file)
  local left, right = {}, {}
  for i, row in ipairs(r) do
    if row.hunk then
      table.insert(left, { spans = { { text = row.text, fg = "cyan", bold = true } } })
      table.insert(right, { spans = { { text = "", fg = "cyan" } } })
    else
      local mark = (i == V.cursor) and ">" or " "
      if row.kind == "del" then
        table.insert(left, { spans = { { text = mark .. "-" .. row.text, fg = "lightred" } } })
        table.insert(right, { spans = { { text = "" } } })
      elseif row.kind == "add" then
        table.insert(left, { spans = { { text = "" } } })
        table.insert(right, { spans = { { text = mark .. "+" .. row.text, fg = "green" } } })
      else
        table.insert(left, { spans = { { text = mark .. " " .. row.text, fg = "gray" } } })
        table.insert(right, { spans = { { text = mark .. " " .. row.text, fg = "gray" } } })
      end
    end
  end
  return {
    type = "split",
    direction = "horizontal",
    children = {
      { type = "list", id = "body", items = left, offset = V.scroll },
      { type = "list", items = right, offset = V.scroll },
    },
  }
end

local function diff_view()
  local f = cur_file()
  if f == nil then
    return { type = "text", content = "(no changes to review)", fg = "darkgray" }
  end

  local head = string.format("%s  %d/%d  %s",
    f.path, V.file, #files(),
    V.split and "split" or "unified")

  local body = V.split and split_body(f) or unified_body(f)

  local children = {
    { type = "text", content = head, fg = "cyan", bold = true,
      size = { fixed = 1 }, wrap = false },
  }
  if V.tree then
    table.insert(children, {
      type = "split", direction = "horizontal",
      children = { file_list(), body },
    })
  else
    table.insert(children, body)
  end

  -- Bottom bar: comment input or help
  if V.typing ~= nil then
    table.insert(children, {
      type = "text", content = "comment> " .. V.typing .. "  (Enter: save, Esc: cancel)",
      fg = "magenta", size = { fixed = 1 }, wrap = false,
    })
  else
    local spans = {}
    if #V.comments > 0 then
      table.insert(spans, { text = "[C-s]", fg = "cyan" })
      table.insert(spans, { text = string.format(" send %d  ", #V.comments), fg = "yellow" })
    end
    table.insert(spans, { text = "[j/k]", fg = "cyan" })
    table.insert(spans, { text = " nav  ", fg = "darkgray" })
    table.insert(spans, { text = "[n/p]", fg = "cyan" })
    table.insert(spans, { text = " file  ", fg = "darkgray" })
    table.insert(spans, { text = "[d]", fg = "cyan" })
    table.insert(spans, { text = " status  ", fg = "darkgray" })
    table.insert(spans, { text = "[c]", fg = "cyan" })
    table.insert(spans, { text = " comment  ", fg = "darkgray" })
    table.insert(spans, { text = "[Esc]", fg = "cyan" })
    table.insert(spans, { text = " close", fg = "darkgray" })
    table.insert(children, {
      type = "text", spans = spans,
      size = { fixed = 1 }, wrap = false,
    })
  end

  return { type = "split", direction = "vertical", children = children }
end

function render(state)
  LAST = state
  if state == nil or state.repo == nil then
    return { type = "text", content = "(not a git repository)", fg = "darkgray" }
  end
  if V.mode == "diff" then
    return diff_view()
  elseif V.mode == "graph" then
    return graph_view(state.repo)
  elseif V.mode == "commit" then
    return commit_view(state.repo)
  end
  return status_view(state.repo)
end
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff;
    use crate::git::{FileChange, LogEntry};

    fn sample_state() -> GitState {
        GitState {
            branch: Some("main".to_string()),
            ahead: 2,
            behind: 0,
            changes: vec![FileChange {
                status: "M".to_string(),
                path: "a.rs".to_string(),
            }],
            recent: vec![LogEntry {
                sha: "abc1234".to_string(),
                subject: "fix".to_string(),
                author: "dev".to_string(),
                date: "2 hours ago".to_string(),
                refs: vec!["HEAD -> main".to_string()],
                graph: "* ".to_string(),
            }],
            refs: vec![],
            stashes: vec![],
        }
    }

    const SAMPLE_DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -10,4 +10,5 @@ fn main() {
 context
-gone
+new one
+new two
";

    fn default_target() -> DiffTarget {
        DiffTarget::WorkingTree
    }

    fn no_commit_diffs() -> CommitDiffs {
        CommitDiffs::new()
    }

    #[test]
    fn not_a_repo_serializes_repo_as_null() {
        let json = state_to_json(None, &[], &default_target(), &no_commit_diffs());
        assert_eq!(json["repo"], serde_json::Value::Null);
        assert_eq!(json["diff"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn state_to_json_round_trips_fields() {
        let json = state_to_json(Some(&sample_state()), &[], &default_target(), &no_commit_diffs());
        assert_eq!(json["repo"]["branch"], "main");
        assert_eq!(json["repo"]["ahead"], 2);
        assert_eq!(json["repo"]["changes"][0]["status"], "M");
        assert_eq!(json["repo"]["recent"][0]["sha"], "abc1234");
    }

    /// The per-line numbers must survive serialization: they are the whole
    /// reason a comment lands on the right line.
    #[test]
    fn diff_json_carries_per_line_numbers() {
        let files = diff::parse(SAMPLE_DIFF);
        let json = state_to_json(Some(&sample_state()), &files, &default_target(), &no_commit_diffs());
        let lines = &json["diff"][0]["hunks"][0]["lines"];

        assert_eq!(lines[0]["kind"], "ctx");
        assert_eq!(lines[0]["new_lineno"], 10);
        // Removed line: absent from the new file, so null rather than a number.
        assert_eq!(lines[1]["kind"], "del");
        assert_eq!(lines[1]["new_lineno"], serde_json::Value::Null);
        assert_eq!(lines[2]["new_lineno"], 11);
        assert_eq!(lines[3]["new_lineno"], 12);
        assert_eq!(json["diff"][0]["additions"], 2);
        assert_eq!(json["diff"][0]["deletions"], 1);
    }

    /// Build a Lua state with the stubs `LUA_SOURCE` expects from the host, so
    /// the chunk can be loaded and exercised without a running TUI.
    fn lua_with_stubs() -> mlua::Lua {
        let lua = mlua::Lua::new();
        let kn9t = lua.create_table().unwrap();
        // Record bindings so tests can invoke them the way the host would.
        let keys = lua.create_table().unwrap();
        let clicks = lua.create_table().unwrap();
        kn9t.set("_keys", keys.clone()).unwrap();
        kn9t.set("_clicks", clicks.clone()).unwrap();
        kn9t.set("_inserted", lua.create_table().unwrap()).unwrap();

        let k = keys.clone();
        kn9t.set(
            "on_key",
            lua.create_function(move |_, (key, f): (String, mlua::Function)| {
                k.set(key, f)?;
                Ok(true)
            })
            .unwrap(),
        )
        .unwrap();

        let c = clicks.clone();
        kn9t.set(
            "on_click",
            lua.create_function(move |_, (id, f): (String, mlua::Function)| {
                c.set(id, f)?;
                Ok(())
            })
            .unwrap(),
        )
        .unwrap();

        kn9t.set(
            "insert_input",
            lua.create_function(|lua, text: String| {
                let kn9t: mlua::Table = lua.globals().get("kn9t")?;
                let inserted: mlua::Table = kn9t.get("_inserted")?;
                inserted.push(text)?;
                Ok(())
            })
            .unwrap(),
        )
        .unwrap();

        kn9t.set(
            "log",
            lua.create_function(|_, _: String| Ok(())).unwrap(),
        )
        .unwrap();

        // Stub for write_file (test only - host provides real one)
        kn9t.set(
            "write_file",
            lua.create_function(|_, (_path, _content): (String, String)| Ok(())).unwrap(),
        )
        .unwrap();

        lua.globals().set("kn9t", kn9t).unwrap();
        lua.load(LUA_SOURCE).exec().expect("LUA_SOURCE must parse");
        lua
    }

    fn press(lua: &mlua::Lua, key: &str) {
        let kn9t: mlua::Table = lua.globals().get("kn9t").unwrap();
        let keys: mlua::Table = kn9t.get("_keys").unwrap();
        let f: mlua::Function = keys
            .get(key)
            .unwrap_or_else(|_| panic!("key '{key}' not bound"));
        f.call::<mlua::Value>(key).unwrap();
    }

    fn render_with(lua: &mlua::Lua, json: &serde_json::Value) -> mlua::Table {
        let render: mlua::Function = lua.globals().get("render").unwrap();
        render.call(json_to_lua_for_test(lua, json)).unwrap()
    }

    fn view_state(lua: &mlua::Lua) -> mlua::Table {
        lua.globals().get("V").unwrap()
    }

    #[test]
    fn render_handles_no_repo_and_status_mode() {
        let lua = lua_with_stubs();

        let w = render_with(&lua, &serde_json::Value::Null);
        assert_eq!(w.get::<String>("type").unwrap(), "text");

        let json = state_to_json(Some(&sample_state()), &[], &default_target(), &no_commit_diffs());
        let w = render_with(&lua, &json);
        assert_eq!(w.get::<String>("type").unwrap(), "split");
    }

    /// `d` switches to the review panel, which is a different tree — proving the
    /// plugin's own key binding drives its own layout with no host involvement.
    #[test]
    fn d_toggles_into_diff_mode() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF), &default_target(), &no_commit_diffs());

        render_with(&lua, &json); // publish state to LAST
        assert_eq!(view_state(&lua).get::<String>("mode").unwrap(), "status");

        press(&lua, "d");
        assert_eq!(view_state(&lua).get::<String>("mode").unwrap(), "diff");
        let w = render_with(&lua, &json);
        assert_eq!(w.get::<String>("type").unwrap(), "split");
    }

    #[test]
    fn cursor_moves_and_clamps_at_both_ends() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF), &default_target(), &no_commit_diffs());
        render_with(&lua, &json);
        press(&lua, "d");

        press(&lua, "j");
        assert_eq!(view_state(&lua).get::<i64>("cursor").unwrap(), 2);
        press(&lua, "k");
        press(&lua, "k");
        assert_eq!(
            view_state(&lua).get::<i64>("cursor").unwrap(),
            1,
            "must not go below the first row"
        );

        // 1 hunk header + 4 lines = 5 rows; walking past the end must clamp.
        for _ in 0..20 {
            press(&lua, "j");
        }
        assert_eq!(view_state(&lua).get::<i64>("cursor").unwrap(), 5);
    }

    #[test]
    fn u_and_b_toggle_split_and_tree() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF), &default_target(), &no_commit_diffs());
        render_with(&lua, &json);
        press(&lua, "d");

        assert!(!view_state(&lua).get::<bool>("split").unwrap());
        press(&lua, "u");
        assert!(view_state(&lua).get::<bool>("split").unwrap());
        // Both bodies must still render.
        render_with(&lua, &json);

        assert!(view_state(&lua).get::<bool>("tree").unwrap());
        press(&lua, "b");
        assert!(!view_state(&lua).get::<bool>("tree").unwrap());
        render_with(&lua, &json);
    }

    /// The end-to-end review workflow, and the reason `insert_input` exists.
    #[test]
    fn comment_flow_anchors_to_the_correct_line_and_sends() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF), &default_target(), &no_commit_diffs());
        render_with(&lua, &json);
        press(&lua, "d");

        // Row 4 is the first added line, which is new-file line 11.
        press(&lua, "j"); // 2: context (line 10)
        press(&lua, "j"); // 3: removed
        press(&lua, "j"); // 4: added -> 11

        press(&lua, "c");
        for ch in ["o", "d", "d"] {
            press(&lua, ch);
        }
        press(&lua, "Enter");

        let comments: mlua::Table = view_state(&lua).get("comments").unwrap();
        assert_eq!(comments.len().unwrap(), 1);
        let first: mlua::Table = comments.get(1).unwrap();
        assert_eq!(first.get::<String>("text").unwrap(), "odd");
        assert_eq!(
            first.get::<i64>("line").unwrap(),
            11,
            "must use the per-line number, not hunk_start + index"
        );

        press(&lua, "C-s");
        let kn9t: mlua::Table = lua.globals().get("kn9t").unwrap();
        let inserted: mlua::Table = kn9t.get("_inserted").unwrap();
        assert_eq!(inserted.len().unwrap(), 1);
        let text: String = inserted.get(1).unwrap();
        assert!(text.contains("src/main.rs:11"), "got: {text}");
        assert!(text.contains("odd"));
        // Sending clears the queue so the next send is not a duplicate.
        let comments: mlua::Table = view_state(&lua).get("comments").unwrap();
        assert_eq!(comments.len().unwrap(), 0);
    }

    /// When focused, all printable keys must be consumed to prevent typing
    /// in the user input. This is different from unfocused mode.
    #[test]
    fn printable_keys_consumed_when_focused() {
        let lua = lua_with_stubs();
        let kn9t: mlua::Table = lua.globals().get("kn9t").unwrap();
        let keys: mlua::Table = kn9t.get("_keys").unwrap();

        // `o` is a plain printable with no other meaning in this view.
        let f: mlua::Function = keys.get("o").unwrap();
        let consumed: mlua::Value = f.call("o").unwrap();
        assert_eq!(
            consumed,
            mlua::Value::Boolean(true),
            "focused: must consume to prevent typing in user input"
        );
    }

    /// A poll must not move the cursor the user just set.
    #[test]
    fn incoming_state_does_not_reset_view_state() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF), &default_target(), &no_commit_diffs());
        render_with(&lua, &json);
        press(&lua, "d");
        press(&lua, "j");
        press(&lua, "u");

        render_with(&lua, &json); // a fresh push arrives

        let v = view_state(&lua);
        assert_eq!(v.get::<String>("mode").unwrap(), "diff");
        assert_eq!(v.get::<i64>("cursor").unwrap(), 2);
        assert!(v.get::<bool>("split").unwrap());
    }

    #[test]
    fn clicking_a_file_row_selects_it() {
        let lua = lua_with_stubs();
        let two = format!(
            "{SAMPLE_DIFF}\
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-x
+y
"
        );
        let json = state_to_json(Some(&sample_state()), &diff::parse(&two), &default_target(), &no_commit_diffs());
        render_with(&lua, &json);
        press(&lua, "d");

        let kn9t: mlua::Table = lua.globals().get("kn9t").unwrap();
        let clicks: mlua::Table = kn9t.get("_clicks").unwrap();
        let f: mlua::Function = clicks.get("files").unwrap();
        f.call::<mlua::Value>((0, 1, "left")).unwrap(); // second row

        assert_eq!(view_state(&lua).get::<i64>("file").unwrap(), 2);
    }

    #[test]
    fn diff_mode_with_no_changes_renders_a_message() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &[], &default_target(), &no_commit_diffs());
        render_with(&lua, &json);
        press(&lua, "d");
        let w = render_with(&lua, &json);
        assert_eq!(w.get::<String>("type").unwrap(), "text");
    }

    /// Minimal JSON->Lua conversion for the tests above. Deliberately NOT the
    /// production path (that lives host-side in kn9t-tui's plugin_ui.rs) — but
    /// it must agree on the one thing that matters here: JSON null becomes nil,
    /// which is how `new_lineno` absence is represented.
    fn json_to_lua_for_test(lua: &mlua::Lua, v: &serde_json::Value) -> mlua::Value {
        match v {
            serde_json::Value::Null => mlua::Value::Nil,
            serde_json::Value::Bool(b) => mlua::Value::Boolean(*b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => mlua::Value::Integer(i),
                None => mlua::Value::Number(n.as_f64().unwrap_or(0.0)),
            },
            serde_json::Value::String(s) => mlua::Value::String(lua.create_string(s).unwrap()),
            serde_json::Value::Array(arr) => {
                let t = lua.create_table().unwrap();
                for (i, item) in arr.iter().enumerate() {
                    t.set(i + 1, json_to_lua_for_test(lua, item)).unwrap();
                }
                mlua::Value::Table(t)
            }
            serde_json::Value::Object(map) => {
                let t = lua.create_table().unwrap();
                for (k, val) in map {
                    t.set(k.as_str(), json_to_lua_for_test(lua, val)).unwrap();
                }
                mlua::Value::Table(t)
            }
        }
    }
}
