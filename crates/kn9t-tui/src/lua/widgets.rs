//! Declarative widget tree.
//!
//! Lua builds a tree of typed widget tables. Rust renders them via ratatui.
//! Lua never emits spans/glyphs directly — it configures widget *properties*.
//!
//! Widget types:
//! - `Text`: Static or dynamic text content
//! - `Box`: Container with border/title
//! - `List`: Scrollable list of items
//! - `Input`: Text input (editing owned by Rust)
//! - `Split`: Horizontal/vertical split container

use mlua::{Lua, Result as LuaResult, Table, Value};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::theme::Theme;

/// Result of asking Lua to build the root UI.
///
/// Three states rather than `Option`, so the renderer can tell "user has no Lua
/// UI" (use the Rust layout) apart from "user's Lua UI is broken" (show a
/// minimal shell plus the error). Falling back to the full Rust chrome on error
/// makes a broken config look like a working TUI, which hides the problem.
#[derive(Debug, Clone)]
pub enum UiOutcome {
    /// No `render_ui` defined: Rust owns the layout.
    NotDefined,
    /// Lua built a valid tree.
    Ok(Widget),
    /// Lua exists but is broken; carries a message to display.
    Failed(String),
}

/// A declarative widget node parsed from Lua.
#[derive(Debug, Clone)]
pub enum Widget {
    /// Static or dynamic text.
    Text {
        /// Styled runs. A plain `content="..."` parses to a single span, so the
        /// renderer has one path and mixed-colour lines are always expressible.
        spans: Vec<TextSpan>,
        style: WidgetStyle,
        wrap: bool,
        align: Align,
        /// Render the text as markdown (headings, code fences, lists, emphasis).
        ///
        /// The parsing/highlighting stays in Rust — Lua only opts in.
        markdown: bool,
        /// Highlight the text as source code in this language (e.g. "rust").
        ///
        /// Ignored when `markdown` is set, since fenced blocks handle it there.
        syntax: Option<String>,
        /// Convert `$...$` LaTeX math to Unicode before rendering.
        math: bool,
        /// Wrap bare URLs in OSC 8 escapes so terminals make them clickable.
        ///
        /// A no-op on terminals that do not advertise support.
        linkify: bool,
    },
    /// Container with optional border, title and padding.
    Box {
        title: Option<String>,
        title_align: Align,
        border: BorderKind,
        border_style: WidgetStyle,
        padding: Padding,
        child: Option<Box<Widget>>,
        style: WidgetStyle,
    },
    /// Scrollable list of items.
    List {
        items: Vec<Vec<TextSpan>>,
        selected: Option<usize>,
        /// First visible row. Lua owns the scroll position; Rust clamps it to
        /// keep the selection on screen so a naive config still behaves.
        offset: usize,
        highlight_style: WidgetStyle,
        style: WidgetStyle,
    },
    /// Text input field (editing state owned by Rust).
    Input {
        id: String,
        placeholder: Option<String>,
        style: WidgetStyle,
    },
    /// Horizontal or vertical split container.
    Split {
        direction: SplitDirection,
        children: Vec<Widget>,
        sizes: Vec<SplitSize>,
    },
    /// Progress/usage bar. A primitive rather than a Lua helper because
    /// `string.rep` cannot express sub-cell fill or a centred label.
    Gauge {
        frac: f64,
        label: Option<String>,
        style: WidgetStyle,
        /// Glyphs for filled/empty cells; defaults to a solid block pair.
        filled: String,
        empty: String,
    },
    /// A child floated over the layout at an explicit position.
    ///
    /// This is what makes popups/toasts expressible in `render_ui` itself,
    /// instead of needing a second placement mechanism.
    Float {
        x: Option<u16>,
        y: Option<u16>,
        width: Option<u16>,
        height: Option<u16>,
        /// Paint the region opaque first, so a float over the transcript reads
        /// as a panel rather than as overlapping glyphs.
        clear: bool,
        child: Option<Box<Widget>>,
    },
    /// A native Rust view embedded in the Lua tree.
    ///
    /// Lua declares *where* it goes; Rust owns *how* it renders.
    /// See `NATIVE_VIEWS` for the list, which Lua can also query at runtime.
    Native { view: String },
    /// A plugin-supplied UI slot.
    ///
    /// The plugin's registered Lua produces the subtree; this node only records
    /// *where* it goes, so a plugin cannot choose its own placement.
    Plugin { plugin: String },
    /// Empty placeholder (for graceful degradation and explicit spacing).
    Empty,
    /// Wraps any widget that was given `id="..."` in its Lua table.
    ///
    /// A transparent pass-through for layout/rendering purposes — it occupies
    /// exactly the rect its child would have, and renders the child unchanged.
    /// It exists purely to carry an id through to `collect_clickable_areas`,
    /// which `kn9t.on_click(id, fn)` dispatch matches against. Wrapping rather
    /// than adding an `id: Option<String>` field to all ten other variants
    /// keeps every existing match arm untouched.
    Clickable { id: String, child: Box<Widget> },
}

/// Native views Lua may place with `{type="native", view=...}`.
///
/// Published to Lua as `kn9t.native_views` so a config can degrade gracefully
/// on an older binary instead of silently leaving a hole in the layout.
///
/// `welcome` is deliberately not here. The welcome screen is chosen by `App::screen`
/// before any Lua runs, and `render_welcome` lays out a centred logo plus a prompt — it is
/// not a view that means anything dropped into the chat layout. Listing it invited a config
/// to place one and get a confusing frame.
pub const NATIVE_VIEWS: &[&str] = &["transcript", "input", "status", "explorer", "viewer"];

impl Widget {
    /// Concatenated text of a `Text` node, ignoring per-span styling.
    ///
    /// For assertions and for the markdown/syntax paths, which operate on plain
    /// text. Returns `None` for any other node type.
    pub fn text(&self) -> Option<String> {
        match self {
            Widget::Text { spans, .. } => Some(join_spans(spans)),
            _ => None,
        }
    }
}

/// Split direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    Horizontal,
    Vertical,
}

/// Split size constraint.
#[derive(Debug, Clone, Copy)]
pub enum SplitSize {
    Fixed(u16),
    Percent(u16),
    Flex(u16),
}

/// Widget styling options.
#[derive(Debug, Clone, Default)]
pub struct WidgetStyle {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub reversed: bool,
}

impl WidgetStyle {
    pub fn to_ratatui_style(&self, theme: &Theme) -> Style {
        let mut style = Style::default();
        style = style.fg(self.fg.unwrap_or(theme.fg));
        if let Some(bg) = self.bg {
            style = style.bg(bg);
        }
        for (on, m) in [
            (self.bold, Modifier::BOLD),
            (self.italic, Modifier::ITALIC),
            (self.underline, Modifier::UNDERLINED),
            (self.dim, Modifier::DIM),
            (self.reversed, Modifier::REVERSED),
        ] {
            if on {
                style = style.add_modifier(m);
            }
        }
        style
    }
}

/// One styled run of text inside a `text` widget.
///
/// Exists because a single `fg` per widget made a mixed-colour line impossible
/// to express: the status bar had to be Rust, and every Lua attempt at one
/// degenerated into a monochrome string.
#[derive(Debug, Clone)]
pub struct TextSpan {
    pub text: String,
    pub style: WidgetStyle,
    /// File path for syntax highlighting (extension is extracted automatically).
    pub syntax: Option<String>,
}

/// Horizontal alignment of a `text` widget's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Border look of a `box`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderKind {
    None,
    #[default]
    Plain,
    Rounded,
    Thick,
    Double,
}

/// Inner padding, in cells: (top, right, bottom, left).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Padding {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl Padding {
    /// Shrink `area` by this padding, saturating so it can never invert.
    pub fn inner(&self, area: Rect) -> Rect {
        let w = area
            .width
            .saturating_sub(self.left.saturating_add(self.right));
        let h = area
            .height
            .saturating_sub(self.top.saturating_add(self.bottom));
        Rect::new(
            area.x + self.left.min(area.width),
            area.y + self.top.min(area.height),
            w,
            h,
        )
    }
}

/// Parse a widget tree from a Lua table.
pub fn parse_widget(lua: &Lua, table: &Table) -> LuaResult<Widget> {
    let widget_type: String = table.get("type").unwrap_or_else(|_| "text".to_string());

    let widget = match widget_type.as_str() {
        "text" => parse_text_widget(table),
        "box" => parse_box_widget(lua, table),
        "list" => parse_list_widget(table),
        "input" => parse_input_widget(table),
        "split" => parse_split_widget(lua, table),
        "gauge" => parse_gauge_widget(table),
        "float" => parse_float_widget(lua, table),
        // Explicit blank space. Cheaper and clearer than an empty text node,
        // which is what configs otherwise resort to for layout gaps.
        "spacer" | "empty" => Ok(Widget::Empty),
        "native" => {
            let view: String = table.get("view").unwrap_or_default();
            Ok(Widget::Native { view })
        }
        // A plugin's own UI. Lua names the plugin and chooses the slot; the
        // plugin's `render(state)` supplies the subtree. Resolved at render
        // time so a plugin registering later still appears without a reload.
        "plugin" => {
            let plugin: String = table.get("plugin").unwrap_or_default();
            Ok(Widget::Plugin { plugin })
        }
        _ => {
            crate::log!("Lua widget: unknown type '{}', using empty", widget_type);
            Ok(Widget::Empty)
        }
    }?;

    // `id="..."` is accepted on ANY widget type, orthogonal to `type=`, so it
    // wraps the parsed result rather than being threaded through every variant
    // above. This is the hook `kn9t.on_click(id, fn)` matches against — see
    // `collect_clickable_areas` and `Widget::Clickable`.
    match table.get::<Option<String>>("id").ok().flatten() {
        Some(id) if !id.is_empty() => Ok(Widget::Clickable {
            id,
            child: Box::new(widget),
        }),
        _ => Ok(widget),
    }
}

/// Parse the styled runs of a `text` or list item.
///
/// Accepts `content="..."` (one span, inheriting the node style),
/// `spans={{text=..., fg=...}, ...}`, or a bare `text="..."`. `text` is accepted
/// as a synonym for `content` because it is the obvious key to reach for and a
/// list item that silently renders empty is a bug that hides in a rendered frame.
fn parse_spans(table: &Table, fallback: &WidgetStyle) -> LuaResult<Vec<TextSpan>> {
    if let Ok(spans_table) = table.get::<Table>("spans") {
        let mut out = Vec::new();
        for i in 1..=spans_table.len()? {
            match spans_table.get::<Value>(i) {
                // A bare string in a spans list inherits the node style.
                Ok(Value::String(s)) => out.push(TextSpan {
                    text: s.to_str()?.to_string(),
                    style: fallback.clone(),
                    syntax: None,
                }),
                Ok(Value::Table(t)) => {
                    let text: String = t.get("text").unwrap_or_default();
                    // A span with no colour of its own must not fall back to the
                    // theme default, or it would visibly differ from its parent.
                    let mut style = parse_style(&t)?;
                    if style.fg.is_none() {
                        style.fg = fallback.fg;
                    }
                    if style.bg.is_none() {
                        style.bg = fallback.bg;
                    }
                    let syntax: Option<String> = t.get("syntax").ok();
                    out.push(TextSpan {
                        text,
                        style,
                        syntax,
                    });
                }
                _ => {}
            }
        }
        return Ok(out);
    }

    let content: String = table
        .get::<String>("content")
        .ok()
        .or_else(|| table.get::<String>("text").ok())
        .unwrap_or_default();
    // The item's own colour wins; fall back to the node style for what it omits.
    let mut style = parse_style(table)?;
    if style.fg.is_none() {
        style.fg = fallback.fg;
    }
    if style.bg.is_none() {
        style.bg = fallback.bg;
    }
    Ok(vec![TextSpan {
        text: content,
        style,
        syntax: None,
    }])
}

fn parse_align(table: &Table, key: &str) -> Align {
    match table.get::<String>(key).unwrap_or_default().as_str() {
        "center" | "centre" => Align::Center,
        "right" => Align::Right,
        _ => Align::Left,
    }
}

/// Parse a `border` field, accepting both a bool and a style name.
///
/// `border=true`/`false` predates the named kinds and stays valid: a config
/// written against the old vocabulary must not break on upgrade.
fn parse_border(table: &Table) -> BorderKind {
    match table.get::<Value>("border") {
        Ok(Value::Boolean(true)) => BorderKind::Plain,
        Ok(Value::Boolean(false)) => BorderKind::None,
        Ok(Value::String(s)) => {
            let name = s.to_str().map(|s| s.to_lowercase()).unwrap_or_default();
            match name.as_str() {
                "none" | "" => BorderKind::None,
                "rounded" | "round" => BorderKind::Rounded,
                "thick" | "bold" => BorderKind::Thick,
                "double" => BorderKind::Double,
                _ => BorderKind::Plain,
            }
        }
        // Absent: bordered, matching the previous default.
        _ => BorderKind::Plain,
    }
}

/// Parse `padding`, accepting a scalar, `{v, h}`, or `{t, r, b, l}`.
fn parse_padding(table: &Table) -> Padding {
    match table.get::<Value>("padding") {
        Ok(Value::Integer(n)) => {
            let n = n.max(0) as u16;
            Padding {
                top: n,
                right: n,
                bottom: n,
                left: n,
            }
        }
        Ok(Value::Table(t)) => {
            let at = |i: i64| t.get::<i64>(i).unwrap_or(0).max(0) as u16;
            match t.len().unwrap_or(0) {
                // {vertical, horizontal} — the common CSS shorthand.
                2 => Padding {
                    top: at(1),
                    right: at(2),
                    bottom: at(1),
                    left: at(2),
                },
                4 => Padding {
                    top: at(1),
                    right: at(2),
                    bottom: at(3),
                    left: at(4),
                },
                _ => {
                    // Named form, for when only one side matters.
                    let named = |k: &str| t.get::<i64>(k).unwrap_or(0).max(0) as u16;
                    Padding {
                        top: named("top"),
                        right: named("right"),
                        bottom: named("bottom"),
                        left: named("left"),
                    }
                }
            }
        }
        _ => Padding::default(),
    }
}

fn parse_text_widget(table: &Table) -> LuaResult<Widget> {
    let style = parse_style(table)?;
    let mut spans = parse_spans(table, &style)?;
    let wrap: bool = table.get("wrap").unwrap_or(true);
    let markdown: bool = table.get("markdown").unwrap_or(false);
    let syntax: Option<String> = table.get("syntax").ok();
    let math: bool = table.get("math").unwrap_or(false);
    let linkify: bool = table.get("linkify").unwrap_or(false);

    // Applied at parse time: both are pure text transforms, so doing them here
    // keeps the render path free of per-frame string work.
    if math || linkify {
        for span in &mut spans {
            if math {
                span.text = crate::latex::process_math(&span.text);
            }
            if linkify {
                span.text = crate::hyperlinks::linkify_urls(&span.text);
            }
        }
    }

    Ok(Widget::Text {
        spans,
        style,
        wrap,
        align: parse_align(table, "align"),
        markdown,
        syntax,
        math,
        linkify,
    })
}

fn parse_box_widget(lua: &Lua, table: &Table) -> LuaResult<Widget> {
    let title: Option<String> = table.get("title").ok();
    let style = parse_style(table)?;

    // Border colour defaults to the box's own, so `fg=` alone tints both.
    let mut border_style = WidgetStyle {
        fg: table
            .get::<String>("border_fg")
            .ok()
            .and_then(|s| crate::theme::parse_color(&s))
            .or(style.fg),
        bg: style.bg,
        ..Default::default()
    };
    if let Ok(b) = table.get::<bool>("border_bold") {
        border_style.bold = b;
    }

    let child = if let Ok(child_table) = table.get::<Table>("child") {
        Some(Box::new(parse_widget(lua, &child_table)?))
    } else {
        None
    };

    Ok(Widget::Box {
        title,
        title_align: parse_align(table, "title_align"),
        border: parse_border(table),
        border_style,
        padding: parse_padding(table),
        child,
        style,
    })
}

fn parse_list_widget(table: &Table) -> LuaResult<Widget> {
    let style = parse_style(table)?;

    let mut items: Vec<Vec<TextSpan>> = Vec::new();
    if let Ok(items_table) = table.get::<Table>("items") {
        for i in 1..=items_table.len()? {
            match items_table.get::<Value>(i) {
                Ok(Value::String(s)) => items.push(vec![TextSpan {
                    text: s.to_str()?.to_string(),
                    style: style.clone(),
                    syntax: None,
                }]),
                // A table item is a styled row: either {text=...} or {spans=...}.
                Ok(Value::Table(t)) => items.push(parse_spans(&t, &style)?),
                _ => {}
            }
        }
    }

    let selected: Option<usize> = table.get::<i64>("selected").ok().map(|i| i.max(0) as usize);
    let offset = table.get::<i64>("offset").unwrap_or(0).max(0) as usize;

    // Selection defaults to reversed, which reads correctly on any colourscheme.
    let highlight_style = if table.contains_key("selected_fg").unwrap_or(false)
        || table.contains_key("selected_bg").unwrap_or(false)
    {
        WidgetStyle {
            fg: table
                .get::<String>("selected_fg")
                .ok()
                .and_then(|s| crate::theme::parse_color(&s)),
            bg: table
                .get::<String>("selected_bg")
                .ok()
                .and_then(|s| crate::theme::parse_color(&s)),
            bold: table.get("selected_bold").unwrap_or(false),
            ..Default::default()
        }
    } else {
        WidgetStyle {
            reversed: true,
            ..Default::default()
        }
    };

    Ok(Widget::List {
        items,
        selected,
        offset,
        highlight_style,
        style,
    })
}

fn parse_input_widget(table: &Table) -> LuaResult<Widget> {
    let id: String = table.get("id").unwrap_or_else(|_| "default".to_string());
    let placeholder: Option<String> = table.get("placeholder").ok();
    let style = parse_style(table)?;

    Ok(Widget::Input {
        id,
        placeholder,
        style,
    })
}

fn parse_gauge_widget(table: &Table) -> LuaResult<Widget> {
    let frac: f64 = table.get("frac").unwrap_or(0.0);
    Ok(Widget::Gauge {
        // Clamp here rather than at render: an out-of-range value is a config
        // bug, and drawing a bar wider than its box would corrupt the frame.
        frac: frac.clamp(0.0, 1.0),
        label: table.get::<String>("label").ok(),
        style: parse_style(table)?,
        filled: table.get::<String>("filled").unwrap_or_else(|_| "█".into()),
        empty: table.get::<String>("empty").unwrap_or_else(|_| "░".into()),
    })
}

fn parse_float_widget(lua: &Lua, table: &Table) -> LuaResult<Widget> {
    let dim = |k: &str| table.get::<i64>(k).ok().map(|v| v.max(0) as u16);
    let child = if let Ok(child_table) = table.get::<Table>("child") {
        Some(Box::new(parse_widget(lua, &child_table)?))
    } else {
        None
    };
    Ok(Widget::Float {
        x: dim("x"),
        y: dim("y"),
        width: dim("width").or_else(|| dim("w")),
        height: dim("height").or_else(|| dim("h")),
        clear: table.get("clear").unwrap_or(true),
        child,
    })
}

fn parse_split_widget(lua: &Lua, table: &Table) -> LuaResult<Widget> {
    let dir_str: String = table
        .get("direction")
        .unwrap_or_else(|_| "vertical".to_string());
    let direction = match dir_str.as_str() {
        "horizontal" | "h" | "row" => SplitDirection::Horizontal,
        _ => SplitDirection::Vertical,
    };

    let mut children = Vec::new();
    let mut sizes = Vec::new();

    if let Ok(children_table) = table.get::<Table>("children") {
        for i in 1..=children_table.len()? {
            if let Ok(child_table) = children_table.get::<Table>(i) {
                children.push(parse_widget(lua, &child_table)?);

                // Parse size for this child
                let size = if let Ok(size_table) = child_table.get::<Table>("size") {
                    parse_size(&size_table)?
                } else {
                    SplitSize::Flex(1)
                };
                sizes.push(size);
            }
        }
    }

    Ok(Widget::Split {
        direction,
        children,
        sizes,
    })
}

fn parse_size(table: &Table) -> LuaResult<SplitSize> {
    if let Ok(fixed) = table.get::<i64>("fixed") {
        return Ok(SplitSize::Fixed(fixed as u16));
    }
    if let Ok(percent) = table.get::<i64>("percent") {
        return Ok(SplitSize::Percent(percent as u16));
    }
    if let Ok(flex) = table.get::<i64>("flex") {
        return Ok(SplitSize::Flex(flex as u16));
    }
    Ok(SplitSize::Flex(1))
}

fn parse_style(table: &Table) -> LuaResult<WidgetStyle> {
    let mut style = WidgetStyle::default();

    // Colours go through the shared theme parser, so `#rgb`, 256-palette
    // indices and `lightgreen` mean the same thing here as in config.toml.
    if let Ok(fg_str) = table.get::<String>("fg") {
        style.fg = crate::theme::parse_color(&fg_str);
    }
    if let Ok(bg_str) = table.get::<String>("bg") {
        style.bg = crate::theme::parse_color(&bg_str);
    }
    for (key, slot) in [
        ("bold", &mut style.bold),
        ("italic", &mut style.italic),
        ("underline", &mut style.underline),
        ("dim", &mut style.dim),
        ("reverse", &mut style.reversed),
    ] {
        if let Ok(v) = table.get::<bool>(key) {
            *slot = v;
        }
    }

    Ok(style)
}

/// Parse a status-bar segment array from Lua.
///
/// Shares [`parse_style`] with the widget tree so a status segment supports the
/// same styling as any `text` node. `color=` is accepted alongside `fg=` because
/// the status bar API shipped with that spelling.
pub fn parse_status_spans(table: &Table) -> Vec<TextSpan> {
    let mut out = Vec::new();
    for i in 1..=table.len().unwrap_or(0) {
        let Ok(seg) = table.get::<Table>(i) else {
            continue;
        };
        let text: String = seg.get("text").unwrap_or_default();
        let mut style = parse_style(&seg).unwrap_or_default();
        if style.fg.is_none() {
            style.fg = seg
                .get::<String>("color")
                .ok()
                .and_then(|s| crate::theme::parse_color(&s));
        }
        out.push(TextSpan {
            text,
            style,
            syntax: None,
        });
    }
    out
}

/// Plain text of a span run, for the markdown/syntax paths.
pub fn join_spans(spans: &[TextSpan]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Convert one span run into a single ratatui line.
///
/// When a span has `syntax` set (a file path), extracts the extension and
/// applies syntax highlighting while preserving background color.
fn spans_to_line(spans: &[TextSpan], theme: &Theme) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for s in spans {
        if let Some(ref path) = s.syntax {
            // Extract language from file path extension
            let lang = std::path::Path::new(path)
                .extension()
                .and_then(|e| e.to_str());
            let highlighted = crate::syntax::highlight_code_inline(&s.text, lang, theme);
            // Preserve background if set
            for mut hl_span in highlighted {
                if let Some(bg) = s.style.bg {
                    hl_span.style = hl_span.style.bg(bg);
                }
                out.push(hl_span);
            }
        } else {
            out.push(Span::styled(
                s.text.clone(),
                s.style.to_ratatui_style(theme),
            ));
        }
    }
    Line::from(out)
}

/// Convert a span run into lines, splitting on embedded newlines.
///
/// Lua builds multi-line content with `\n` inside a span (see `kv_lines`), so
/// this has to break lines itself rather than assume one span is one line.
fn spans_to_lines(spans: &[TextSpan], theme: &Theme) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();

    for span in spans {
        let style = span.style.to_ratatui_style(theme);
        let mut parts = span.text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                current.push(Span::styled(part.to_string(), style));
            }
            // Every separator but the last closes the current line.
            if parts.peek().is_some() {
                lines.push(Line::from(std::mem::take(&mut current)));
            }
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(Line::from(current));
    }
    lines
}

/// Border glyph set for a [`BorderKind`], or `None` for no border.
fn border_set(kind: BorderKind) -> Option<ratatui::symbols::border::Set> {
    use ratatui::symbols::border;
    match kind {
        BorderKind::None => None,
        BorderKind::Plain => Some(border::PLAIN),
        BorderKind::Rounded => Some(border::ROUNDED),
        BorderKind::Thick => Some(border::THICK),
        BorderKind::Double => Some(border::DOUBLE),
    }
}

/// Resolve a [`Widget::Float`]'s rect within `area`.
///
/// Shared by the renderer and the geometry walk: if these disagreed, a click on
/// a floating panel would be tested against a rect nobody drew.
fn float_area(widget: &Widget, area: Rect) -> Rect {
    let Widget::Float {
        x,
        y,
        width,
        height,
        ..
    } = widget
    else {
        return area;
    };

    // Default to 1/2 of the parent, which is a usable popup at any terminal size.
    let w = width.unwrap_or(area.width / 2).min(area.width);
    let h = height.unwrap_or(area.height / 2).min(area.height);
    // Unspecified position centres; an explicit one is clamped so a float can
    // never be pushed off-screen by a bad number.
    let px = x
        .map(|v| area.x.saturating_add(v.min(area.width.saturating_sub(w))))
        .unwrap_or(area.x + (area.width.saturating_sub(w)) / 2);
    let py = y
        .map(|v| area.y.saturating_add(v.min(area.height.saturating_sub(h))))
        .unwrap_or(area.y + (area.height.saturating_sub(h)) / 2);
    Rect::new(px, py, w, h)
}

/// Render a widget tree to a ratatui Frame.
///
/// `Widget::Native` nodes are *not* drawn here — they are placeholders whose
/// geometry is resolved by [`collect_natives`] and rendered by the caller
/// (which owns `&mut App`).
pub fn render_widget(
    f: &mut Frame,
    widget: &Widget,
    area: Rect,
    theme: &Theme,
    input_states: &std::collections::HashMap<String, String>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    match widget {
        Widget::Text {
            spans,
            style,
            wrap,
            align,
            markdown,
            syntax,
            ..
        } => {
            // Markdown / syntax highlighting are Rust mechanisms Lua opts into.
            // Both operate on plain text, so they use the concatenated spans.
            let para = if *markdown {
                let lines = crate::markdown::render(&join_spans(spans), theme, area.width as usize);
                Paragraph::new(lines)
            } else if let Some(lang) = syntax {
                let lines = crate::syntax::highlight_code(
                    &join_spans(spans),
                    Some(lang.as_str()),
                    theme,
                    Style::default().fg(theme.muted),
                );
                Paragraph::new(lines)
            } else {
                // Newlines inside spans still split lines, so a multi-line
                // styled block behaves like the single-string form used to.
                Paragraph::new(spans_to_lines(spans, theme)).style(style.to_ratatui_style(theme))
            };

            let para = para.alignment(match align {
                Align::Left => ratatui::layout::Alignment::Left,
                Align::Center => ratatui::layout::Alignment::Center,
                Align::Right => ratatui::layout::Alignment::Right,
            });
            let para = if *wrap && !*markdown && syntax.is_none() {
                para.wrap(Wrap { trim: false })
            } else {
                para
            };
            f.render_widget(para, area);
        }

        Widget::Box {
            title,
            title_align,
            border,
            border_style,
            padding,
            child,
            style,
        } => {
            let mut block = Block::default().style(style.to_ratatui_style(theme));
            if let Some(set) = border_set(*border) {
                block = block
                    .borders(Borders::ALL)
                    .border_set(set)
                    .border_style(border_style.to_ratatui_style(theme));
            }
            if let Some(t) = title {
                block = block.title(t.as_str()).title_alignment(match title_align {
                    Align::Left => ratatui::layout::Alignment::Left,
                    Align::Center => ratatui::layout::Alignment::Center,
                    Align::Right => ratatui::layout::Alignment::Right,
                });
            }

            let inner = padding.inner(block.inner(area));
            f.render_widget(block, area);

            if let Some(child_widget) = child {
                render_widget(f, child_widget, inner, theme, input_states);
            }
        }

        Widget::List {
            items,
            selected,
            offset,
            highlight_style,
            style,
        } => {
            // Clamp the offset so the selection stays visible: Lua owns scroll,
            // but a config that forgets to follow the cursor should not render a
            // list with nothing selected on screen.
            let height = area.height as usize;
            let mut start = (*offset).min(items.len().saturating_sub(1).max(0));
            if let Some(sel) = *selected {
                if sel < start {
                    start = sel;
                } else if height > 0 && sel >= start + height {
                    start = sel + 1 - height;
                }
            }

            let list_items: Vec<ListItem> = items
                .iter()
                .enumerate()
                .skip(start)
                .take(height.max(1))
                .map(|(i, spans)| {
                    let mut line = spans_to_line(spans, theme);
                    if Some(i) == *selected {
                        let hl = highlight_style.to_ratatui_style(theme);
                        // Patch rather than replace, so a selected row keeps the
                        // per-span colours the config gave it.
                        line = Line::from(
                            line.spans
                                .into_iter()
                                .map(|s| {
                                    let patched = s.style.patch(hl);
                                    Span::styled(s.content, patched)
                                })
                                .collect::<Vec<_>>(),
                        );
                    }
                    ListItem::new(line)
                })
                .collect();

            let list = List::new(list_items).style(style.to_ratatui_style(theme));
            f.render_widget(list, area);
        }

        Widget::Gauge {
            frac,
            label,
            style,
            filled,
            empty,
        } => {
            let w = area.width as usize;
            let fill_char = filled.chars().next().unwrap_or('#');
            let empty_char = empty.chars().next().unwrap_or('-');
            let n = ((*frac) * w as f64).round() as usize;
            let mut bar: String = std::iter::repeat_n(fill_char, n.min(w))
                .chain(std::iter::repeat_n(empty_char, w.saturating_sub(n)))
                .collect();

            // Overlay the label centred, so it reads on both halves of the bar.
            if let Some(text) = label {
                let text: String = text.chars().take(w).collect();
                let start = (w.saturating_sub(text.chars().count())) / 2;
                let mut chars: Vec<char> = bar.chars().collect();
                for (i, ch) in text.chars().enumerate() {
                    if let Some(slot) = chars.get_mut(start + i) {
                        *slot = ch;
                    }
                }
                bar = chars.into_iter().collect();
            }

            let para = Paragraph::new(bar).style(style.to_ratatui_style(theme));
            f.render_widget(para, area);
        }

        // Geometry is resolved by `float_area`, shared with the hit-test walk so
        // clicks land where the float was actually drawn.
        Widget::Float { clear, child, .. } => {
            let rect = float_area(widget, area);
            if rect.width == 0 || rect.height == 0 {
                return;
            }
            if *clear {
                f.render_widget(ratatui::widgets::Clear, rect);
            }
            if let Some(child_widget) = child {
                render_widget(f, child_widget, rect, theme, input_states);
            }
        }

        Widget::Input {
            id,
            placeholder,
            style,
        } => {
            let value = input_states.get(id).map(|s| s.as_str()).unwrap_or("");
            let display = if value.is_empty() {
                placeholder.as_deref().unwrap_or("")
            } else {
                value
            };

            let s = if value.is_empty() {
                style
                    .to_ratatui_style(theme)
                    .add_modifier(Modifier::ITALIC)
                    .fg(theme.muted)
            } else {
                style.to_ratatui_style(theme)
            };

            let para = Paragraph::new(display).style(s);
            f.render_widget(para, area);
        }

        Widget::Split {
            direction,
            children,
            sizes,
        } => {
            if children.is_empty() {
                return;
            }

            let rects = compute_split_rects(area, *direction, sizes);

            for (i, child) in children.iter().enumerate() {
                if let Some(rect) = rects.get(i) {
                    render_widget(f, child, *rect, theme, input_states);
                }
            }
        }

        // Placeholders — geometry resolved by the collectors, drawn by the
        // caller, which owns the native views and the plugin registry.
        Widget::Native { .. } | Widget::Plugin { .. } => {}

        Widget::Empty => {}

        // Transparent: renders exactly as its child would, at the same rect.
        // The id only matters to `collect_clickable_areas`.
        Widget::Clickable { child, .. } => {
            render_widget(f, child, area, theme, input_states);
        }
    }
}

/// Resolve the screen rect of every [`Widget::Native`] in the tree.
///
/// Pure geometry: no `Frame` needed, so the Lua-driven layout is unit-testable.
/// Order matches a depth-first walk, which is the order Lua declared them in.
pub fn collect_natives(widget: &Widget, area: Rect, out: &mut Vec<(String, Rect)>) {
    collect_slots(widget, area, out, &mut Vec::new());
}

/// Resolve the screen rect of every `id="..."` widget, for click dispatch.
///
/// A separate walk rather than a third accumulator on `collect_slots`: that
/// function's early-return leaves (`Text`, `List`, `Input`, `Gauge`, `Empty`)
/// are exactly the common targets for a click (a list row, a text label), so
/// this one must recurse into them too — folding it into `collect_slots` would
/// mean every one of its leaf arms stops being a no-op.
///
/// Recurses into a `Clickable`'s own child as well, so a clickable region that
/// wraps a `Box` containing further clickable rows still finds all of them.
pub fn collect_clickable_areas(widget: &Widget, area: Rect, out: &mut Vec<(String, Rect)>) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if let Widget::Clickable { id, child } = widget {
        out.push((id.clone(), area));
        collect_clickable_areas(child, area, out);
        return;
    }

    match widget {
        Widget::Box {
            border,
            padding,
            child,
            ..
        } => {
            if let Some(child_widget) = child {
                let block = match border_set(*border) {
                    Some(_) => Block::default().borders(Borders::ALL),
                    None => Block::default(),
                };
                let inner = padding.inner(block.inner(area));
                collect_clickable_areas(child_widget, inner, out);
            }
        }
        Widget::Float { child, .. } => {
            if let Some(child_widget) = child {
                collect_clickable_areas(child_widget, float_area(widget, area), out);
            }
        }
        Widget::Split {
            direction,
            children,
            sizes,
        } => {
            let rects = compute_split_rects(area, *direction, sizes);
            for (i, child) in children.iter().enumerate() {
                if let Some(rect) = rects.get(i) {
                    collect_clickable_areas(child, *rect, out);
                }
            }
        }
        // Leaves: nothing further to recurse into. Reached either directly
        // (not wrapped in an id) or after the `Clickable` arm above already
        // recorded this rect and recursed once.
        Widget::Text { .. }
        | Widget::List { .. }
        | Widget::Input { .. }
        | Widget::Gauge { .. }
        | Widget::Native { .. }
        | Widget::Plugin { .. }
        | Widget::Empty
        | Widget::Clickable { .. } => {}
    }
}

/// Resolve the screen rect of every [`Widget::Plugin`] slot.
///
/// Separate accessor so callers that only care about plugin placement do not
/// have to filter native views out of a mixed list.
pub fn collect_plugin_slots(widget: &Widget, area: Rect, out: &mut Vec<(String, Rect)>) {
    collect_slots(widget, area, &mut Vec::new(), out);
}

/// One walk that records both native views and plugin slots.
///
/// Kept single so the two cannot disagree about geometry — a plugin slot must
/// land exactly where the same tree would have put a native view.
fn collect_slots(
    widget: &Widget,
    area: Rect,
    natives: &mut Vec<(String, Rect)>,
    plugins: &mut Vec<(String, Rect)>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    match widget {
        Widget::Native { view } => natives.push((view.clone(), area)),

        Widget::Plugin { plugin } => plugins.push((plugin.clone(), area)),

        Widget::Box {
            border,
            padding,
            child,
            ..
        } => {
            if let Some(child_widget) = child {
                // Must mirror the renderer exactly: border inset, then padding.
                let block = match border_set(*border) {
                    Some(_) => Block::default().borders(Borders::ALL),
                    None => Block::default(),
                };
                let inner = padding.inner(block.inner(area));
                collect_slots(child_widget, inner, natives, plugins);
            }
        }

        // A float's child is placed in the float's own rect, so a native view or
        // plugin slot inside a popup reports the popup's geometry.
        Widget::Float { child, .. } => {
            if let Some(child_widget) = child {
                collect_slots(child_widget, float_area(widget, area), natives, plugins);
            }
        }

        Widget::Split {
            direction,
            children,
            sizes,
        } => {
            let rects = compute_split_rects(area, *direction, sizes);
            for (i, child) in children.iter().enumerate() {
                if let Some(rect) = rects.get(i) {
                    collect_slots(child, *rect, natives, plugins);
                }
            }
        }

        Widget::Text { .. }
        | Widget::List { .. }
        | Widget::Input { .. }
        | Widget::Gauge { .. }
        | Widget::Empty => {}

        // Transparent for placement purposes, same as for rendering: a native
        // view or plugin slot wrapped in an id must still be found here.
        Widget::Clickable { child, .. } => {
            collect_slots(child, area, natives, plugins);
        }
    }
}

/// Compute rects for split children.
pub fn compute_split_rects(
    area: Rect,
    direction: SplitDirection,
    sizes: &[SplitSize],
) -> Vec<Rect> {
    if sizes.is_empty() {
        return Vec::new();
    }

    let total = match direction {
        SplitDirection::Horizontal => area.width,
        SplitDirection::Vertical => area.height,
    };

    // Calculate sizes
    let mut fixed_total: u16 = 0;
    let mut flex_total: u16 = 0;

    for size in sizes {
        match size {
            SplitSize::Fixed(n) => fixed_total += n,
            SplitSize::Percent(p) => fixed_total += (total as u32 * *p as u32 / 100) as u16,
            SplitSize::Flex(f) => flex_total += f,
        }
    }

    let remaining = total.saturating_sub(fixed_total);
    let flex_unit = if flex_total > 0 {
        remaining / flex_total
    } else {
        0
    };

    let mut rects = Vec::with_capacity(sizes.len());
    let mut offset: u16 = 0;

    for size in sizes {
        let len = match size {
            SplitSize::Fixed(n) => *n,
            SplitSize::Percent(p) => (total as u32 * *p as u32 / 100) as u16,
            SplitSize::Flex(f) => flex_unit * f,
        };

        // Clamp to parent bounds: a Lua layout that requests more space than
        // exists must not produce rects outside the buffer. Without this,
        // ratatui panics on the out-of-bounds write.
        let rect = match direction {
            SplitDirection::Horizontal => {
                let x = (area.x + offset).min(area.x + area.width);
                let w = len.min(area.width.saturating_sub(offset));
                Rect::new(x, area.y, w, area.height)
            }
            SplitDirection::Vertical => {
                let y = (area.y + offset).min(area.y + area.height);
                let h = len.min(area.height.saturating_sub(offset));
                Rect::new(area.x, y, area.width, h)
            }
        };

        rects.push(rect);
        offset += len;
    }

    rects
}
