//! Chrome layout harness (PLAN §P7 L1).
//!
//! Renders the built-in UI into a `TestBackend` so the chrome can be inspected and locked
//! without a live server, a session, or the `tui-control` MCP. Snapshots are asserted as text:
//! a chrome regression is a diff, not a JPEG a human has to remember.
//!
//! Run with output to eyeball a frame:
//!
//! ```text
//! cargo test -p kn9t-tui --test chrome_layout -- --nocapture
//! ```

use std::sync::Arc;

use kn9t_tui::app::{App, Overlay, Screen};
use kn9t_tui::config::Config;
use kn9t_tui::event::TickControl;
use kn9t_tui::lua::LuaRuntime;
use kn9t_tui::message_handler::{Message, ToolCard};
use kn9t_tui::model_selector::{ModelEntry, ModelSelector};
use kn9t_tui::session_manager::SessionEntry;
use kn9t_tui::theme::Theme;
use kn9t_tui::token_tracker::TokenCounts;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;

const CURRENT: &str = "01M2NGCFZ0000000000000000";

/// A chat screen with plausible state (PLAN §P7 L1), without drawing it.
///
/// Split from `draw` so a test can inspect the `App` *after* a frame — click targets
/// only exist once something has been rendered.
fn chat_app(tweak: impl FnOnce(&mut App)) -> App {
    let rt = LuaRuntime::new().expect("lua runtime");
    // Order matters and is not obvious: the real startup publishes the palette
    // *before* the config runs (`App::init_lua`), so `kn9t.theme` exists at the
    // moment the built-in `00_theme.lua` builds its `C` table. Reversed, every `T.x or
    // "fallback"` silently takes the fallback and the frame is drawn in ANSI
    // colours the theme never chose — a wrong frame that still looks plausible.
    rt.install_environment(&Theme::dark());
    rt.load_builtin();

    let mut app = App::new(Config::default(), TickControl::dummy());
    app.screen = Screen::Chat;
    app.plugins_ready = true;
    app.lua_runtime = Some(Arc::new(rt));

    // Session identity and cwd: what the tab bar and the breadcrumb read.
    app.session.state.session_id = CURRENT.into();
    app.session.state.session_title = Some("TUI polish".into());
    app.session.state.cwd = Some("C:\\_ddm\\projects\\Agents\\kn9t-public".into());
    app.session.state.lease = Some("lease-1-abcd".into());
    app.session.sessions = vec![
        SessionEntry {
            id: CURRENT.into(),
            name: "TUI polish".into(),
            ..Default::default()
        },
        SessionEntry {
            id: "01M2NG7Q0000000000000000".into(),
            name: "Provider audit and three bugs".into(),
            ..Default::default()
        },
        SessionEntry {
            id: "01M2NFGZ0000000000000000".into(),
            name: "Inspect Cargo.toml package".into(),
            ..Default::default()
        },
    ];

    // A model, so the breadcrumb and the sidebar title have something to name.
    app.model_sel = ModelSelector {
        models: vec![ModelEntry {
            provider: "opencode-go".into(),
            id: "deepseek-v4.1-flash".into(),
            api_id: None,
            is_default: true,
            ctx_window: Some(1_000_000),
            max_out: Some(32_768),
        }],
        selected: 0,
    };

    // Mid-session numbers.
    app.tokens.session_totals = TokenCounts::new(310_000, 12_400, 120_000, 0);
    app.tokens.last_turn = TokenCounts::new(48_000, 900, 120_000, 0);
    app.tokens.cost = 0.0842;
    app.tokens.last_toks_per_sec = Some(86.0);

    tweak(&mut app);
    app
}

/// Draw `app` into a `TestBackend`, returning the frame as text and its buffer.
fn draw(app: &mut App, width: u16, height: u16) -> (String, Buffer) {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|f| kn9t_tui::ui::render::render(f, app))
        .expect("draw");
    let buf = terminal.backend().buffer().clone();
    (buffer_to_string(&buf), buf)
}

fn chat_frame(width: u16, height: u16, tweak: impl FnOnce(&mut App)) -> (String, Buffer) {
    draw(&mut chat_app(tweak), width, height)
}

/// A tool card in the state the transcript normally holds it.
fn card(call_id: &str, name: &str, args: &str, status: &str) -> ToolCard {
    ToolCard {
        call_id: call_id.into(),
        name: name.into(),
        args: args.into(),
        status: status.into(),
        output: None,
        progress_lines: Vec::new(),
        expanded: false,
        active_tab: Default::default(),
        scroll_offset: 0,
    }
}

/// Does any cell of `row` carry `bg`? Proves a style actually reached the screen
/// rather than merely being present in the widget tree.
fn row_has_bg(buf: &Buffer, row: u16, bg: ratatui::style::Color) -> bool {
    let area = buf.area;
    (area.left()..area.right()).any(|x| buf[(x, row)].bg == bg)
}

fn row_text(buf: &Buffer, row: u16) -> String {
    let area = buf.area;
    (area.left()..area.right())
        .map(|x| buf[(x, row)].symbol())
        .collect::<String>()
        .trim_end()
        .to_string()
}

fn buffer_to_string(buf: &Buffer) -> String {
    let area = buf.area;
    let mut out = String::new();
    for y in area.top()..area.bottom() {
        out.push_str(&row_text(buf, y));
        out.push('\n');
    }
    out
}

/// The header is two rows and each row carries one fact (PLAN §P7 D14).
#[test]
fn header_is_tab_bar_over_breadcrumb() {
    let (frame, buf) = chat_frame(190, 44, |_| {});
    println!("\n=== 190x44 ===\n{frame}");

    let tabs = row_text(&buf, 0);
    let crumbs = row_text(&buf, 1);

    // Row 1: the open sessions, the current one first, plus the new-tab button.
    assert!(tabs.contains("TUI polish"), "tab bar row: {tabs:?}");
    assert!(tabs.contains("Provider audit"), "tab bar row: {tabs:?}");
    assert!(tabs.contains("+ new"), "tab bar row: {tabs:?}");

    // Row 2: where we are. The breadcrumb is cwd, then the session title.
    assert!(crumbs.contains("_ddm"), "breadcrumb row: {crumbs:?}");
    assert!(crumbs.contains("▸"), "breadcrumb row: {crumbs:?}");
    assert!(
        crumbs.contains("TUI polish"),
        "the session title belongs in the breadcrumb: {crumbs:?}"
    );
    // The model is the prompt frame's title (D12). It used to be here *and* in the status
    // bar *and* in the right panel's title *and* on the frame — four copies of one string.
    assert!(
        !crumbs.contains("deepseek-v4.1-flash"),
        "the model must not be repeated in the breadcrumb: {crumbs:?}"
    );

    // The active tab is a solid accent block, not merely bold text.
    assert!(
        row_has_bg(&buf, 0, Theme::dark().primary),
        "the active tab must be drawn as an accent block"
    );
}

/// The status bar is segmented and the context chip is a coloured block (D13).
#[test]
fn status_bar_is_segmented() {
    let (_, buf) = chat_frame(190, 44, |_| {});
    let row = buf.area.bottom() - 1;
    let status = row_text(&buf, row);
    println!("status: {status:?}");

    for expected in ["ctx", "$0.0842", "commands"] {
        assert!(
            status.contains(expected),
            "status bar missing {expected:?}: {status:?}"
        );
    }
    // The model lives on the prompt frame, not here (see `input_is_a_boxed_prompt`).
    assert!(
        !status.contains("deepseek-v4.1-flash"),
        "the model must not be repeated on the status bar: {status:?}"
    );
    // A chip has a background; a plain text line never does.
    let ok = Theme::dark().success;
    let mut with_bg = String::new();
    for y in buf.area.top()..buf.area.bottom() {
        if row_has_bg(&buf, y, ok) {
            with_bg.push_str(&format!("{y} "));
        }
    }
    assert!(
        !with_bg.is_empty(),
        "the context chip under 75% must be drawn on the success colour; rows with it: {with_bg:?}; status: {status:?}"
    );
}

/// D7: the right panel keeps context/usage/transcript and drops the tool list,
/// which duplicated the transcript's own cards one column over.
#[test]
fn right_panel_drops_recent_calls() {
    let (frame, _) = chat_frame(190, 44, |_| {});
    assert!(frame.contains("usage"), "the usage panel is still wanted");
    assert!(
        !frame.contains("recent calls"),
        "`recent calls` was dropped by D7"
    );
}

/// D3: sessions live in the tab bar, so there is no session column to duplicate them.
#[test]
fn there_is_no_session_column() {
    let (frame, buf) = chat_frame(190, 44, |_| {});
    assert!(!frame.contains("Sessions"), "no session panel: {frame}");

    // Below the header and above the input frame, the body starts at the left
    // edge. A column there would show as a border in the first cell of every row
    // — the input frame's own edge is excluded, since it legitimately sits at x=0.
    let input_top = (buf.area.top()..buf.area.bottom())
        .find(|&y| row_text(&buf, y).contains('╭'))
        .expect("input frame");
    for y in 2..input_top {
        let left = row_text(&buf, y).chars().next();
        assert!(
            left != Some('│') && left != Some('┌') && left != Some('└'),
            "row {y} starts with a column border, so something still owns the left edge"
        );
    }
}

/// D12: the prompt is a frame whose top border names the model.
#[test]
fn input_is_a_boxed_prompt() {
    let (frame, buf) = chat_frame(190, 44, |_| {});
    assert!(
        frame.contains("› "),
        "the prompt marker moved but must still exist"
    );

    // Find the frame's top border by its corner glyph *and* its title: the sidebars are boxes
    // too now, so "the first row with a ╭" is not the prompt.
    let top = (buf.area.top()..buf.area.bottom())
        .find(|&y| {
            let r = row_text(&buf, y);
            r.contains('╭') && r.contains("deepseek-v4.1-flash")
        })
        .expect("the input is drawn as a box titled with the model");
    let top_row = row_text(&buf, top);

    assert!(
        top_row.contains("deepseek-v4.1-flash"),
        "the model belongs in the frame's title: {top_row:?}"
    );
    assert!(
        top_row.contains("Enter send"),
        "the frame should say how to send: {top_row:?}"
    );
    // Content sits inside the frame, not under it.
    let inside = row_text(&buf, top + 1);
    assert!(inside.starts_with('│'), "the frame's left edge: {inside:?}");
    assert!(
        inside.contains('›'),
        "the prompt lives inside the frame: {inside:?}"
    );
}

/// A turn's tool calls read as one group, and the group header is a real click target.
///
/// The second half is the part that matters: a header a user can see but not click is
/// exactly the bug class this repo has hit before (a binding that parses and dispatches
/// nowhere). Asserting the string is on screen proves nothing about that.
#[test]
fn a_turn_groups_its_tool_calls() {
    let tools = || {
        vec![
            card(
                "t1",
                "bash",
                r#"{"cmd":"Get-ChildItem crates\\kn9t-tui\\src"}"#,
                "done",
            ),
            card(
                "t2",
                "read",
                r#"{"path":"crates/kn9t-tui/src/theme.rs"}"#,
                "done",
            ),
            card(
                "t3",
                "edit",
                r#"{"path":"crates/kn9t-tui/src/app.rs","old_string":"a","new_string":"b"}"#,
                "done",
            ),
        ]
    };

    let mut app = chat_app(|app| {
        app.transcript.messages_mut().push(Message {
            role: "assistant".into(),
            content: "Working on it.".into(),
            tools: tools(),
            thinking: Vec::new(),
            image_count: 0,
        });
    });
    let (frame, _) = draw(&mut app, 190, 44);
    println!("\n=== collapsed turn ===\n{frame}");

    // Collapsed: a rollup header, then one compact line per call.
    assert!(frame.contains("3 tools"), "the turn rolls up: {frame}");
    assert!(frame.contains("Get-ChildItem"), "bash line missing");
    assert!(frame.contains("theme.rs"), "read line missing");
    assert!(frame.contains("app.rs"), "edit line missing");
    assert!(
        frame.contains("+1 -1"),
        "an edit must show what it changed: {frame}"
    );

    // The header must own the group, so clicking it can expand the turn. A header that
    // is visible but not a click target is the failure mode this asserts against.
    let group = app
        .tool_hit_areas
        .iter()
        .find(|h| !h.group_calls.is_empty())
        .expect("the group header must be a click target");
    assert_eq!(
        group.group_calls,
        vec!["t1", "t2", "t3"],
        "the header must own every call in the turn"
    );

    // Expanding the group replaces the compact lines with the cards themselves.
    let group_calls = group.group_calls.clone();
    app.toggle_tool_group(&group_calls);
    let (expanded, _) = draw(&mut app, 190, 44);
    println!("\n=== expanded turn ===\n{expanded}");
    assert!(
        expanded.contains("-a") && expanded.contains("+b"),
        "the expanded edit must show its diff: {expanded}"
    );
}

/// D18: an overlay is a framed, themed panel — not a hardcoded black slab.
///
/// The overlays painted `Color::Black` literally in ~25 places, so on a light terminal
/// every picker was a black box. This asserts the *cells*, because that is where the bug
/// was: the code looked themed and was not.
#[test]
fn overlays_are_themed_and_framed() {
    let mut app = chat_app(|app| {
        app.overlay = Some(Overlay::ModelSelect {
            selected: 0,
            filter: String::new(),
        });
    });
    let (frame, buf) = draw(&mut app, 120, 40);
    println!("\n=== model picker ===\n{frame}");

    assert!(frame.contains('╭'), "an overlay must be drawn as a frame");
    assert!(frame.contains("SELECT MODEL"), "the title is still there");

    // No cell may carry a literal black: `ink` is a near-black, not `Black`, so a match
    // means a hardcoded colour survived.
    let area = buf.area;
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let c = &buf[(x, y)];
            assert_ne!(
                c.fg,
                ratatui::style::Color::Black,
                "hardcoded fg black at ({x},{y})"
            );
            assert_ne!(
                c.bg,
                ratatui::style::Color::Black,
                "hardcoded bg black at ({x},{y})"
            );
        }
    }
}

#[test]
fn tiny_input_falls_back_to_a_bare_prompt() {
    let (frame, _) = chat_frame(6, 8, |_| {});
    println!("\n=== 6x8 ===\n{frame}");
    assert!(
        !frame.contains('╭'),
        "no frame when there is no room for one"
    );
}

/// A narrow terminal must give up the panels rather than squeeze the centre.
#[test]
fn narrow_frame_hides_sidebars() {
    let (frame, buf) = chat_frame(80, 24, |_| {});
    println!("\n=== 80x24 ===\n{frame}");
    assert!(
        !frame.contains("usage"),
        "the sidebar must yield below the min width"
    );
    assert!(
        frame.contains("deepseek-v4.1-flash"),
        "the prompt frame must still name the model at 80 columns"
    );
    assert!(buf.area.width == 80);
}

/// Every tab must be a real click target carrying the same `id` the Lua handler binds.
///
/// This is a named failure mode in this repo: `on_click("tab_<session>")` was registered for
/// every session while the tabs were spans inside a single text node — a span has no rect,
/// so `collect_clickable_areas` never recorded one and no click could ever reach the
/// handler. Asserting the label is on screen proves nothing about that; this asserts the
/// geometry *and* that a handler is bound to the same id.
#[test]
fn session_tabs_are_click_targets() {
    let mut app = chat_app(|_| {});
    let (frame, _) = draw(&mut app, 190, 44);
    println!("\n=== tab bar ===\n{frame}");

    // Bindings queued by `kn9t.on_click` during the frame are drained by the app loop.
    let rt = app.lua_runtime.clone().expect("lua runtime");
    rt.drain_clicks(&mut app.lua_clicks);

    let tab_ids: Vec<String> = app
        .session
        .sessions
        .iter()
        .map(|s| format!("tab_{}", s.id))
        .collect();
    assert!(tab_ids.len() >= 2, "the fixture opens several sessions");

    for id in &tab_ids {
        let rect = app
            .lua_click_areas
            .iter()
            .find(|(i, _)| i == id)
            .map(|(_, r)| *r)
            .unwrap_or_else(|| panic!("no clickable rect for {id}: {:?}", app.lua_click_areas));
        assert_eq!(rect.y, 0, "{id} must be on the tab row, got {rect:?}");
        assert!(rect.width > 0, "{id} rect: {rect:?}");
        assert!(
            app.lua_clicks.has(id),
            "{id} has a rect but no Lua handler bound to it"
        );
    }

    // And the button keeps working the same way.
    assert!(
        app.lua_click_areas.iter().any(|(i, _)| i == "tab_new"),
        "the `+ new` button must stay a click target: {:?}",
        app.lua_click_areas
    );
    assert!(app.lua_clicks.has("tab_new"), "`+ new` lost its handler");
}

/// The `@` finder must work on the welcome screen, not only inside a session (PLAN §P7 L2).
///
/// The welcome screen draws its own input rather than the native `input` view, so it needed its
/// own dropdown call: the finder opened in a session and did nothing before one was picked.
#[test]
fn the_mention_dropdown_opens_on_the_welcome_screen() {
    use kn9t_tui::file_index::FileIndex;

    let mut app = chat_app(|app| {
        app.screen = Screen::Welcome;
        app.file_index = FileIndex::from_paths_at(
            "C:\\work",
            ["crates/kn9t-tui/src/app.rs", "docs/design.md"]
                .iter()
                .map(|s| s.to_string()),
        );
        app.input = "@".into();
        app.cursor_col = 1;
        app.mention
            .sync(&app.file_index, &app.input, app.cursor_col);
    });

    let (frame, _) = draw(&mut app, 120, 40);
    println!("\n=== welcome + mention ===\n{frame}");

    assert!(app.mention.active, "a bare `@` must open the finder");
    assert!(
        frame.contains("app.rs") || frame.contains("design.md"),
        "the welcome screen must draw the mention rows, got:\n{frame}"
    );
}

/// The viewer must show which line `c` will reference, and mark it.
#[test]
fn the_viewer_marks_the_cursor_and_shows_the_reference() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut viewer = kn9t_tui::viewer::ViewerState::open(root, "Cargo.toml").expect("opens");
    viewer.move_cursor(4); // line 5
    let mut app = chat_app(|app| app.viewer = Some(viewer));

    let (frame, buf) = draw(&mut app, 160, 44);
    println!("\n=== viewer cursor ===\n{frame}");

    assert!(
        frame.contains("L5 · c comment"),
        "the status line must name the line and the action, got:\n{frame}"
    );
    let (vx, vy, _, _) = app.viewer_area.expect("viewer rect");
    assert_eq!(
        buf[(vx + 1, vy + 1 + 4)].symbol(),
        "▌",
        "the cursor line needs a marker"
    );
}

/// The explorer column appears when `kn9t.state.explorer_visible` is set (PLAN §P7 L2 / D3).
#[test]
fn the_explorer_column_appears_when_opened() {
    let mut app = chat_app(|app| app.explorer.focus());
    let (frame, _) = draw(&mut app, 190, 44);
    println!("\n=== explorer ===\n{frame}");

    assert!(frame.contains("files"), "the column has no title");
    assert!(
        app.explorer_area.is_some(),
        "the explorer rect was not recorded, so clicks cannot hit it"
    );
    // An index that has not walked yet is a stated empty state, not a blank column.
    assert!(frame.contains("(no files)"), "an empty index must say so");
}

/// The viewer stacks above the transcript and shows the file (PLAN §P7 L2 / D4/D5).
#[test]
fn the_viewer_stacks_above_the_transcript() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let viewer = kn9t_tui::viewer::ViewerState::open(root, "Cargo.toml")
        .expect("the crate manifest is readable");

    let mut app = chat_app(|app| app.viewer = Some(viewer));
    let (frame, _) = draw(&mut app, 160, 44);
    println!("\n=== viewer ===\n{frame}");

    assert!(
        frame.contains("Cargo.toml"),
        "the header must name the file"
    );
    assert!(
        frame.contains("lines"),
        "the header must state the line count"
    );
    assert!(
        frame.contains("[package]"),
        "the file body must be drawn, got:\n{frame}"
    );
    assert!(
        app.viewer_area.is_some(),
        "the viewer rect was not recorded, so the wheel cannot scroll it"
    );
}
