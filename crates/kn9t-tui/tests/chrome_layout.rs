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
    // moment `default_tui.lua` builds its `C` table. Reversed, every `T.x or
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

    // Row 2: where we are. The breadcrumb is cwd, title, model.
    assert!(crumbs.contains("_ddm"), "breadcrumb row: {crumbs:?}");
    assert!(crumbs.contains("▸"), "breadcrumb row: {crumbs:?}");
    assert!(
        crumbs.contains("deepseek-v4.1-flash"),
        "the model belongs in the breadcrumb, not only in the sidebar: {crumbs:?}"
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

    for expected in ["ctx", "$0.0842", "deepseek-v4.1-flash", "commands"] {
        assert!(status.contains(expected), "status bar missing {expected:?}: {status:?}");
    }
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
    assert!(!frame.contains("recent calls"), "`recent calls` was dropped by D7");
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
    assert!(frame.contains("› "), "the prompt marker moved but must still exist");

    // Find the frame's top border by its corner glyph.
    let top = (buf.area.top()..buf.area.bottom())
        .find(|&y| row_text(&buf, y).contains('╭'))
        .expect("the input is drawn as a box");
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
    assert!(inside.contains('›'), "the prompt lives inside the frame: {inside:?}");
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
    assert!(!frame.contains('╭'), "no frame when there is no room for one");
}

/// A narrow terminal must give up the panels rather than squeeze the centre.
#[test]
fn narrow_frame_hides_sidebars() {
    let (frame, buf) = chat_frame(80, 24, |_| {});
    println!("\n=== 80x24 ===\n{frame}");
    assert!(!frame.contains("usage"), "the sidebar must yield below the min width");
    assert!(
        frame.contains("deepseek-v4.1-flash"),
        "the breadcrumb must survive at 80 columns"
    );
    assert!(buf.area.width == 80);
}
