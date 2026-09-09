// Extracted from src/lua/widgets.rs — the #[cfg(test)] mod tests block.
// Unit tests for the declarative widget tree parser and geometry helpers.
// See src/lua/widgets.rs module-level doc for the widget vocabulary (96E-43).

#![allow(clippy::unwrap_used)]

use kn9t_tui::lua::widgets::{
    Align, SplitDirection, SplitSize, TextSpan, Widget, WidgetStyle,
    collect_clickable_areas, collect_natives, compute_split_rects, join_spans, parse_widget,
    render_widget,
};
use kn9t_tui::theme::Theme;
use mlua::{Lua, Table};
use ratatui::layout::Rect;
use ratatui::style::Color;

#[test]
fn test_parse_native_widget() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("type", "native").unwrap();
    t.set("view", "transcript").unwrap();

    let w = parse_widget(&lua, &t).unwrap();
    match w {
        Widget::Native { view } => assert_eq!(view, "transcript"),
        other => panic!("expected Native, got {:?}", other),
    }
}

#[test]
fn test_native_collects_placement_instead_of_drawing() {
    // A split of two native views must report both rects, in order.
    let lua = Lua::new();
    let spec = r#"
        return {
          type = "split", direction = "vertical",
          children = {
            { type = "native", view = "transcript", size = { flex = 1 } },
            { type = "native", view = "status", size = { fixed = 1 } },
          }
        }
    "#;
    let t: Table = lua.load(spec).eval().unwrap();
    let w = parse_widget(&lua, &t).unwrap();

    let area = Rect::new(0, 0, 40, 10);
    let mut natives = Vec::new();
    collect_natives(&w, area, &mut natives);

    assert_eq!(natives.len(), 2);
    assert_eq!(natives[0].0, "transcript");
    assert_eq!(natives[1].0, "status");
    // status is pinned to the last row, transcript takes the rest.
    assert_eq!(natives[1].1, Rect::new(0, 9, 40, 1));
    assert_eq!(natives[0].1, Rect::new(0, 0, 40, 9));
}

/// `id="..."` must wrap the parsed widget, not replace it — a text node
/// with an id must still render (and report placement) as that text node.
#[test]
fn id_wraps_the_widget_without_changing_its_type() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("type", "text").unwrap();
    t.set("content", "hello").unwrap();
    t.set("id", "greeting").unwrap();

    let w = parse_widget(&lua, &t).unwrap();
    match w {
        Widget::Clickable { id, child } => {
            assert_eq!(id, "greeting");
            assert!(matches!(*child, Widget::Text { .. }));
        }
        other => panic!("expected Clickable wrapping Text, got {:?}", other),
    }
}

/// An empty string id must not register a click target — otherwise a Lua
/// config with `id = base_id or ""` (a common "maybe unset" idiom) would
/// silently create an empty-string handler that swallows nothing useful
/// but still allocates a wrapper node every frame.
#[test]
fn empty_string_id_is_not_wrapped() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("type", "text").unwrap();
    t.set("content", "x").unwrap();
    t.set("id", "").unwrap();

    let w = parse_widget(&lua, &t).unwrap();
    assert!(matches!(w, Widget::Text { .. }), "empty id must not wrap");
}

/// The whole point of `id=`: `collect_clickable_areas` must resolve the
/// exact rect a real layout (split + box + list) actually gives the
/// widget, in local-to-parent terms the renderer also uses.
#[test]
fn collect_clickable_areas_resolves_nested_widget_rects() {
    let lua = Lua::new();
    let spec = r#"
        return {
          type = "split", direction = "vertical",
          children = {
            { type = "text", content = "header", size = { fixed = 1 } },
            {
              type = "box", border = "plain", size = { flex = 1 },
              child = { type = "list", id = "session_list", items = {"a", "b"} },
            },
          }
        }
    "#;
    let t: Table = lua.load(spec).eval().unwrap();
    let w = parse_widget(&lua, &t).unwrap();

    let area = Rect::new(0, 0, 20, 10);
    let mut clicks = Vec::new();
    collect_clickable_areas(&w, area, &mut clicks);

    assert_eq!(clicks.len(), 1);
    assert_eq!(clicks[0].0, "session_list");
    // Row 0 is the header (fixed=1); the box starts at y=1 and, like the
    // renderer, ANY drawn border (including "plain") insets by one cell
    // on every side — so the list's rect starts at y=2, not y=1.
    assert_eq!(clicks[0].1, Rect::new(1, 2, 18, 7));
}

/// Two ids in a split must both be found, each with its own rect — proves
/// the walk does not stop after the first match.
#[test]
fn collect_clickable_areas_finds_multiple_ids() {
    let lua = Lua::new();
    let spec = r#"
        return {
          type = "split", direction = "horizontal",
          children = {
            { type = "text", id = "left", content = "L", size = { fixed = 10 } },
            { type = "text", id = "right", content = "R", size = { flex = 1 } },
          }
        }
    "#;
    let t: Table = lua.load(spec).eval().unwrap();
    let w = parse_widget(&lua, &t).unwrap();

    let area = Rect::new(0, 0, 30, 5);
    let mut clicks = Vec::new();
    collect_clickable_areas(&w, area, &mut clicks);

    assert_eq!(clicks.len(), 2);
    assert_eq!(clicks[0].0, "left");
    assert_eq!(clicks[0].1, Rect::new(0, 0, 10, 5));
    assert_eq!(clicks[1].0, "right");
    assert_eq!(clicks[1].1, Rect::new(10, 0, 20, 5));
}

/// A native view wrapped in an id must still be found by both walks:
/// placement (`collect_natives`) for rendering, AND clickable areas for
/// dispatch. Neither walk may treat `Clickable` as opaque.
#[test]
fn clickable_native_view_is_found_by_both_walks() {
    let lua = Lua::new();
    let spec = r#"
        return { type = "native", view = "transcript", id = "main_transcript" }
    "#;
    let t: Table = lua.load(spec).eval().unwrap();
    let w = parse_widget(&lua, &t).unwrap();

    let area = Rect::new(0, 0, 20, 10);

    let mut natives = Vec::new();
    collect_natives(&w, area, &mut natives);
    assert_eq!(natives, vec![("transcript".to_string(), area)]);

    let mut clicks = Vec::new();
    collect_clickable_areas(&w, area, &mut clicks);
    assert_eq!(clicks, vec![("main_transcript".to_string(), area)]);
}

#[test]
fn test_parse_color_named() {
    assert_eq!(kn9t_tui::theme::parse_color("red"), Some(Color::Red));
    assert_eq!(kn9t_tui::theme::parse_color("Blue"), Some(Color::Blue));
    assert_eq!(kn9t_tui::theme::parse_color("GRAY"), Some(Color::Gray));
}

#[test]
fn test_parse_color_hex() {
    assert_eq!(
        kn9t_tui::theme::parse_color("#ff0000"),
        Some(Color::Rgb(255, 0, 0))
    );
    assert_eq!(
        kn9t_tui::theme::parse_color("#00FF00"),
        Some(Color::Rgb(0, 255, 0))
    );
}

#[test]
fn test_parse_text_widget() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("type", "text").unwrap();
    t.set("content", "Hello").unwrap();
    t.set("bold", true).unwrap();
    t.set("wrap", true).unwrap();

    let widget = parse_widget(&lua, &t).unwrap();
    match &widget {
        Widget::Text {
            style,
            wrap,
            markdown,
            syntax,
            ..
        } => {
            assert_eq!(widget.text().unwrap(), "Hello");
            assert!(style.bold);
            assert!(wrap);
            // Rich rendering is opt-in: plain text by default.
            assert!(!markdown);
            assert!(syntax.is_none());
        }
        _ => panic!("Expected Text widget"),
    }
}

/// `content=` and `spans=` must be interchangeable ways to say the same
/// thing, so adding a colour to part of a line is a local edit.
#[test]
fn text_accepts_styled_spans() {
    let lua = Lua::new();
    let t: Table = lua
        .load(
            r#"return { type = "text", fg = "gray", spans = {
                 { text = "ctx " },
                 { text = "87%", fg = "lightred", bold = true },
               } }"#,
        )
        .eval()
        .unwrap();

    match parse_widget(&lua, &t).unwrap() {
        Widget::Text { spans, .. } => {
            assert_eq!(spans.len(), 2);
            // A span without its own colour inherits the node's, not the theme's.
            assert_eq!(spans[0].style.fg, Some(Color::Gray));
            assert_eq!(spans[1].style.fg, Some(Color::LightRed));
            assert!(spans[1].style.bold);
        }
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn text_widget_opts_into_markdown_and_syntax() {
    let lua = Lua::new();

    let md: Table = lua
        .load(r##"return { type = "text", content = "# hi", markdown = true }"##)
        .eval()
        .unwrap();
    match parse_widget(&lua, &md).unwrap() {
        Widget::Text { markdown, .. } => assert!(markdown),
        other => panic!("expected Text, got {other:?}"),
    }

    let code: Table = lua
        .load(r#"return { type = "text", content = "fn m(){}", syntax = "rust" }"#)
        .eval()
        .unwrap();
    match parse_widget(&lua, &code).unwrap() {
        Widget::Text { syntax, .. } => assert_eq!(syntax.as_deref(), Some("rust")),
        other => panic!("expected Text, got {other:?}"),
    }
}

#[test]
fn markdown_flag_actually_changes_rendering() {
    use ratatui::{Terminal, backend::TestBackend};

    // A heading must not render its '#' marker once markdown is enabled.
    fn draw(markdown: bool) -> String {
        let w = Widget::Text {
            spans: vec![TextSpan {
                text: "# Title".to_string(),
                style: WidgetStyle::default(),
                syntax: None,
            }],
            style: WidgetStyle::default(),
            wrap: true,
            align: Align::Left,
            markdown,
            syntax: None,
            math: false,
            linkify: false,
        };
        let mut term = Terminal::new(TestBackend::new(20, 3)).unwrap();
        let theme = Theme::dark();
        let states = std::collections::HashMap::new();
        term.draw(|f| render_widget(f, &w, Rect::new(0, 0, 20, 3), &theme, &states))
            .unwrap();
        let buf = term.backend().buffer().clone();
        (0..3)
            .map(|y| (0..20).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("")
    }

    assert!(draw(false).contains('#'), "plain text keeps the marker");
    assert!(!draw(true).contains('#'), "markdown consumes the marker");
    assert!(draw(true).contains("Title"), "heading text survives");
}

#[test]
fn test_parse_list_widget() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("type", "list").unwrap();

    let items = lua.create_table().unwrap();
    items.set(1, "Item 1").unwrap();
    items.set(2, "Item 2").unwrap();
    t.set("items", items).unwrap();
    t.set("selected", 1).unwrap();

    let widget = parse_widget(&lua, &t).unwrap();
    match widget {
        Widget::List {
            items, selected, ..
        } => {
            assert_eq!(items.len(), 2);
            assert_eq!(join_spans(&items[0]), "Item 1");
            assert_eq!(selected, Some(1));
        }
        _ => panic!("Expected List widget"),
    }
}

#[test]
fn test_parse_split_widget() {
    let lua = Lua::new();
    let t = lua.create_table().unwrap();
    t.set("type", "split").unwrap();
    t.set("direction", "horizontal").unwrap();

    let children = lua.create_table().unwrap();
    let child1 = lua.create_table().unwrap();
    child1.set("type", "text").unwrap();
    child1.set("content", "Left").unwrap();
    children.set(1, child1).unwrap();

    let child2 = lua.create_table().unwrap();
    child2.set("type", "text").unwrap();
    child2.set("content", "Right").unwrap();
    children.set(2, child2).unwrap();

    t.set("children", children).unwrap();

    let widget = parse_widget(&lua, &t).unwrap();
    match widget {
        Widget::Split {
            direction,
            children,
            sizes: _,
        } => {
            assert_eq!(direction, SplitDirection::Horizontal);
            assert_eq!(children.len(), 2);
        }
        _ => panic!("Expected Split widget"),
    }
}

#[test]
fn test_compute_split_rects_horizontal() {
    let area = Rect::new(0, 0, 100, 10);
    let sizes = vec![
        SplitSize::Fixed(20),
        SplitSize::Flex(1),
        SplitSize::Fixed(30),
    ];
    let rects = compute_split_rects(area, SplitDirection::Horizontal, &sizes);

    assert_eq!(rects.len(), 3);
    assert_eq!(rects[0], Rect::new(0, 0, 20, 10));
    assert_eq!(rects[1], Rect::new(20, 0, 50, 10)); // 100 - 20 - 30 = 50
    assert_eq!(rects[2], Rect::new(70, 0, 30, 10));
}

#[test]
fn test_compute_split_rects_vertical() {
    let area = Rect::new(0, 0, 80, 24);
    let sizes = vec![SplitSize::Percent(50), SplitSize::Percent(50)];
    let rects = compute_split_rects(area, SplitDirection::Vertical, &sizes);

    assert_eq!(rects.len(), 2);
    assert_eq!(rects[0].height, 12);
    assert_eq!(rects[1].height, 12);
}
