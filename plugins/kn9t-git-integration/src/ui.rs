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

use crate::diff::DiffFile;
use crate::git::GitState;

/// JSON pushed via `ui_set_state`.
///
/// `repo` is `null` (not an empty object) when `cwd` is not a git repository,
/// so the Lua side can render a distinct message instead of an
/// empty-but-misleadingly-"clean" status.
pub fn state_to_json(state: Option<&GitState>, files: &[DiffFile]) -> serde_json::Value {
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
            })).collect::<Vec<_>>(),
        }),
    };

    serde_json::json!({
        "repo": repo,
        "diff": files.iter().map(diff_file_to_json).collect::<Vec<_>>(),
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
  mode = "status",   -- "status" | "diff"
  split = false,     -- side-by-side vs unified, diff mode only
  tree = true,       -- show the file list
  file = 1,          -- 1-based index into state.diff
  cursor = 1,        -- 1-based index into the flattened line list
  scroll = 0,
  comments = {},     -- { {path=, line=, text=} }
  typing = nil,      -- in-progress comment text, nil when not composing
  height = 20,       -- last known viewport height, for paging
}

local LAST = nil  -- most recent state, so handlers can see repo data

local function files()
  if LAST == nil or LAST.diff == nil then return {} end
  return LAST.diff
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

local function clamp_cursor(n)
  if V.cursor < 1 then V.cursor = 1 end
  if n > 0 and V.cursor > n then V.cursor = n end
  -- Keep the cursor inside the viewport, accounting for the header row.
  local view = math.max(1, V.height - 2)
  if V.cursor <= V.scroll then V.scroll = V.cursor - 1 end
  if V.cursor > V.scroll + view then V.scroll = V.cursor - view end
  if V.scroll < 0 then V.scroll = 0 end
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
    if V.typing ~= nil and #key == 1 then
      V.typing = V.typing .. key
      return true
    end
    return fn()
  end)
end

bind("j", function() V.cursor = V.cursor + 1; clamp_cursor(#rows(cur_file())) end)
bind("k", function() V.cursor = V.cursor - 1; clamp_cursor(#rows(cur_file())) end)
bind("Down", function() V.cursor = V.cursor + 1; clamp_cursor(#rows(cur_file())) end)
bind("Up", function() V.cursor = V.cursor - 1; clamp_cursor(#rows(cur_file())) end)

bind("PageDown", function()
  V.cursor = V.cursor + math.max(1, V.height - 2)
  clamp_cursor(#rows(cur_file()))
end)
bind("PageUp", function()
  V.cursor = V.cursor - math.max(1, V.height - 2)
  clamp_cursor(#rows(cur_file()))
end)

bind("n", function()
  local f = files()
  if V.file < #f then V.file = V.file + 1; V.cursor = 1; V.scroll = 0 end
end)
bind("p", function()
  if V.file > 1 then V.file = V.file - 1; V.cursor = 1; V.scroll = 0 end
end)

-- Next/previous hunk header, falling through to the next/previous file when
-- there are no more in this one — the same behaviour the Rust viewer had.
bind("]", function()
  local r = rows(cur_file())
  for i = V.cursor + 1, #r do
    if r[i].hunk then V.cursor = i; clamp_cursor(#r); return end
  end
  local f = files()
  if V.file < #f then V.file = V.file + 1; V.cursor = 1; V.scroll = 0 end
end)
bind("[", function()
  local r = rows(cur_file())
  for i = V.cursor - 1, 1, -1 do
    if r[i].hunk then V.cursor = i; clamp_cursor(#r); return end
  end
  if V.file > 1 then
    V.file = V.file - 1
    local pr = rows(cur_file())
    V.cursor = #pr > 0 and #pr or 1
    clamp_cursor(#pr)
  end
end)

bind("u", function() V.split = not V.split end)
bind("b", function() V.tree = not V.tree end)
bind("d", function()
  V.mode = (V.mode == "diff") and "status" or "diff"
  V.cursor = 1
  V.scroll = 0
end)

-- Comment capture. `c` opens composition; printable keys then accumulate via
-- the `bind` wrapper; Enter commits. Esc is deliberately unbound: the host uses
-- it to blur the panel, and trapping it would leave no way out.
bind("c", function()
  if V.mode ~= "diff" then return false end
  V.typing = ""
end)

-- Enter and Backspace are not single printable keys, so they reach here even
-- while composing and mean "commit" / "erase" rather than text.
bind("Enter", function()
  if V.typing == nil then return false end
  local f = cur_file()
  local r = rows(f)
  local row = r[V.cursor]
  if f ~= nil and row ~= nil and V.typing ~= "" then
    -- Anchor to the new-file line where there is one, else the old-file line,
    -- so a comment on a deleted line still lands somewhere meaningful.
    local line = row.new_lineno or row.old_lineno
    if line ~= nil then
      table.insert(V.comments, { path = f.path, line = line, text = V.typing })
    end
  end
  V.typing = nil
end)

bind("Backspace", function()
  if V.typing == nil then return false end
  V.typing = string.sub(V.typing, 1, -2)
end)

-- Hand the collected review to the prompt. This is the one host mutation a
-- plugin view can request, and the workflow the whole panel exists for.
bind("C-s", function()
  if #V.comments == 0 then return false end
  local parts = {}
  for _, c in ipairs(V.comments) do
    table.insert(parts, "[" .. c.path .. ":" .. c.line .. "] " .. c.text)
  end
  kn9t.insert_input(table.concat(parts, "\n"))
  V.comments = {}
end)

-- Remaining printable characters, so composing can type them. Skips anything
-- already bound above — those already append via the `bind` wrapper. When not
-- composing these return false, so the letters keep their host meaning.
local PRINTABLE = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.,:;!?/()<>-_=+*#@'\"`~$%^&|\\"
for i = 1, #PRINTABLE do
  local ch = string.sub(PRINTABLE, i, i)
  if not BOUND[ch] then
    bind(ch, function() return false end)
  end
end

-- Space is sent as "Space" by the TUI, not " ". Bind it separately so typing
-- comments works. The bind wrapper appends " " when V.typing is active.
kn9t.on_key("Space", function()
  if V.typing ~= nil then
    V.typing = V.typing .. " "
    return true
  end
  return false
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

local function status_view(repo)
  local out = {}
  local branch = repo.branch or "?"
  local ab = ""
  if (repo.ahead or 0) > 0 then ab = ab .. " +" .. repo.ahead end
  if (repo.behind or 0) > 0 then ab = ab .. " -" .. repo.behind end
  table.insert(out, { type = "text", content = branch .. ab, fg = "cyan", bold = true,
                      size = { fixed = 1 }, wrap = false })

  local changes = repo.changes or {}
  if #changes == 0 then
    table.insert(out, { type = "text", content = "(clean)", fg = "darkgray",
                        size = { fixed = 1 }, wrap = false })
  else
    for _, c in ipairs(changes) do
      local col = "yellow"
      if c.status == "?" then col = "green"
      elseif c.status == "D" then col = "lightred" end
      table.insert(out, { type = "text", content = c.status .. " " .. c.path, fg = col,
                          size = { fixed = 1 }, wrap = false })
    end
  end

  table.insert(out, { type = "text", content = "", size = { fixed = 1 } })
  table.insert(out, { type = "text", content = "recent:", fg = "darkgray",
                      size = { fixed = 1 }, wrap = false })
  for _, l in ipairs(repo.recent or {}) do
    table.insert(out, { type = "text", content = l.sha .. " " .. l.subject, fg = "gray",
                        size = { fixed = 1 }, wrap = false })
  end
  return { type = "split", direction = "vertical", children = out }
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
    local help = "j/k:move  n/p:file  [/]:hunk  u:split  b:tree  c/click:comment  d:status"
    if #V.comments > 0 then
      help = string.format("C-s:send %d  |  %s", #V.comments, help)
    end
    table.insert(children, {
      type = "text", content = help, fg = "darkgray",
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
            }],
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

    #[test]
    fn not_a_repo_serializes_repo_as_null() {
        let json = state_to_json(None, &[]);
        assert_eq!(json["repo"], serde_json::Value::Null);
        assert_eq!(json["diff"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn state_to_json_round_trips_fields() {
        let json = state_to_json(Some(&sample_state()), &[]);
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
        let json = state_to_json(Some(&sample_state()), &files);
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

        let json = state_to_json(Some(&sample_state()), &[]);
        let w = render_with(&lua, &json);
        assert_eq!(w.get::<String>("type").unwrap(), "split");
    }

    /// `d` switches to the review panel, which is a different tree — proving the
    /// plugin's own key binding drives its own layout with no host involvement.
    #[test]
    fn d_toggles_into_diff_mode() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF));

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
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF));
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
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF));
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
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF));
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

    /// Letters must only be swallowed while composing, or the panel would break
    /// every other binding the moment it had focus.
    #[test]
    fn printable_keys_fall_through_unless_composing() {
        let lua = lua_with_stubs();
        let kn9t: mlua::Table = lua.globals().get("kn9t").unwrap();
        let keys: mlua::Table = kn9t.get("_keys").unwrap();

        // `o` is a plain printable with no other meaning in this view.
        let f: mlua::Function = keys.get("o").unwrap();
        let consumed: mlua::Value = f.call("o").unwrap();
        assert_eq!(
            consumed,
            mlua::Value::Boolean(false),
            "not composing: must fall through to the host"
        );
    }

    /// A poll must not move the cursor the user just set.
    #[test]
    fn incoming_state_does_not_reset_view_state() {
        let lua = lua_with_stubs();
        let json = state_to_json(Some(&sample_state()), &diff::parse(SAMPLE_DIFF));
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
        let json = state_to_json(Some(&sample_state()), &diff::parse(&two));
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
        let json = state_to_json(Some(&sample_state()), &[]);
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
