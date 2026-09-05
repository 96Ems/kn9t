//! Benchmarks for TUI rendering performance.
//!
//! Run with: cargo test -p kn9t-tui bench_ --release -- --nocapture
//! Or in debug: cargo test -p kn9t-tui bench_ -- --nocapture

use std::time::{Duration, Instant};

use ratatui::text::Line;

use crate::message_handler::{Message, ToolCard, ToolTab, Transcript};
use crate::render_cache::RenderCache;
use crate::theme::Theme;

/// Generate a realistic assistant message with markdown content.
fn generate_assistant_message(idx: usize) -> Message {
    let content = format!(
        r#"## Response {}

Here's an explanation of the code:

```rust
fn main() {{
    let x = {};
    println!("Hello, world! {{}}", x);
    for i in 0..10 {{
        do_something(i);
    }}
}}
```

This code does the following:
1. **Declares** a variable `x` with value {}
2. **Prints** a greeting message
3. **Iterates** through a loop

> Note: This is an important note about the implementation.
> Make sure to handle errors properly.

The complexity is O(n) where n is the input size. Here's a table:

| Operation | Time | Space |
|-----------|------|-------|
| Insert    | O(1) | O(1)  |
| Delete    | O(n) | O(1)  |
| Search    | O(n) | O(1)  |

For more information, see [the documentation](https://example.com).
"#,
        idx, idx, idx
    );

    Message::new("assistant", content)
}

/// Generate a user message.
fn generate_user_message(idx: usize) -> Message {
    Message::new(
        "user",
        format!(
            "Can you help me with task {}? I need to implement a feature that does X, Y, and Z. \
             The requirements are: first do A, then B, and finally C. Make sure to handle edge cases.",
            idx
        ),
    )
}

/// Generate a message with tool cards.
fn generate_tool_message(idx: usize) -> Message {
    let mut msg = Message::new("assistant", format!("Running tool for task {}...", idx));
    msg.tools.push(ToolCard {
        call_id: format!("call-{}", idx),
        name: "bash".into(),
        args: r#"{"cmd": "cargo build --release"}"#.into(),
        status: "done".into(),
        output: Some("Compiling...\nFinished release [optimized] target(s) in 12.34s".into()),
        progress_lines: vec![
            "Compiling kn9t v0.1.0".into(),
            "Compiling kn9t-core v0.1.0".into(),
            "Compiling kn9t-tui v0.1.0".into(),
        ],
        expanded: true,
        active_tab: ToolTab::Output,
        scroll_offset: 0,
    });
    msg
}

/// Build a transcript with N messages.
fn build_transcript(n: usize) -> Transcript {
    let mut transcript = Transcript::new();
    for i in 0..n {
        if i % 3 == 0 {
            transcript.push(generate_user_message(i));
        } else if i % 3 == 1 {
            transcript.push(generate_assistant_message(i));
        } else {
            transcript.push(generate_tool_message(i));
        }
    }
    transcript
}

/// Measure time to execute a closure N times.
fn bench<F: FnMut()>(name: &str, iterations: usize, mut f: F) -> Duration {
    // Warmup
    for _ in 0..3 {
        f();
    }

    let start = Instant::now();
    for _ in 0..iterations {
        f();
    }
    let elapsed = start.elapsed();
    let per_iter = elapsed / iterations as u32;

    println!(
        "  {}: {:?} total, {:?} per iteration ({} iters)",
        name, elapsed, per_iter, iterations
    );

    elapsed
}

/// Benchmark markdown parsing (the expensive part).
#[test]
fn bench_markdown_parsing() {
    println!("\n=== Markdown Parsing Benchmark ===");

    let theme = Theme::dark();
    let content = generate_assistant_message(42).content;
    let width = 80usize;

    // Short content
    let short = "Hello **world**! This is `code`.";
    bench("short markdown (50 chars)", 1000, || {
        let _ = crate::markdown::render(short, &theme, width);
    });

    // Medium content (typical message)
    bench("medium markdown (~1KB)", 100, || {
        let _ = crate::markdown::render(&content, &theme, width);
    });

    // Long content (multiple code blocks, tables)
    let long_content = (0..5)
        .map(|i| generate_assistant_message(i).content)
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    bench("long markdown (~5KB)", 20, || {
        let _ = crate::markdown::render(&long_content, &theme, width);
    });
}

/// Benchmark syntax highlighting.
#[test]
fn bench_syntax_highlighting() {
    println!("\n=== Syntax Highlighting Benchmark ===");

    let theme = Theme::dark();
    let line_style = ratatui::style::Style::default();

    let short_code = "fn main() { println!(\"Hello\"); }";
    bench("short code (1 line)", 1000, || {
        let _ = crate::syntax::highlight_code(short_code, Some("rust"), &theme, line_style);
    });

    let medium_code = r#"
fn main() {
    let x = 42;
    for i in 0..10 {
        println!("{}: {}", i, x);
    }
}
"#;
    bench("medium code (7 lines)", 500, || {
        let _ = crate::syntax::highlight_code(medium_code, Some("rust"), &theme, line_style);
    });

    let long_code = (0..50)
        .map(|i| format!("    let var_{} = {};", i, i * 2))
        .collect::<Vec<_>>()
        .join("\n");
    let long_code = format!("fn main() {{\n{}\n}}", long_code);
    bench("long code (52 lines)", 50, || {
        let _ = crate::syntax::highlight_code(&long_code, Some("rust"), &theme, line_style);
    });
}

/// Benchmark render cache effectiveness.
#[test]
fn bench_render_cache() {
    println!("\n=== Render Cache Benchmark ===");

    let theme = Theme::dark();
    let width = 80usize;
    let transcript = build_transcript(20);

    // Simulate rendering WITHOUT cache (always parse)
    let no_cache_time = bench("render WITHOUT cache (20 msgs)", 10, || {
        let mut lines: Vec<Line> = Vec::new();
        for msg in transcript.messages() {
            if msg.role == "assistant" && !msg.content.is_empty() {
                let md_lines = crate::markdown::render(&msg.content, &theme, width);
                for line in md_lines {
                    lines.push(line);
                }
            }
        }
    });

    // Simulate rendering WITH cache (first pass populates, subsequent are cache hits)
    let mut cache = RenderCache::new();

    // First pass - populate cache
    for (idx, msg) in transcript.messages().iter().enumerate() {
        if msg.role == "assistant" && !msg.content.is_empty() {
            let md_lines = crate::markdown::render(&msg.content, &theme, width);
            let lines: Vec<Line<'static>> = md_lines.into_iter().collect();
            let tool_info_hash = crate::render_cache::compute_tool_info_hash(&msg.tools);
            cache.set_message(idx, &msg.content, tool_info_hash, lines, vec![]);
        }
    }

    // Subsequent passes - cache hits
    let with_cache_time = bench("render WITH cache (20 msgs, warm)", 10, || {
        let mut lines: Vec<Line> = Vec::new();
        for (idx, msg) in transcript.messages().iter().enumerate() {
            if msg.role == "assistant" && !msg.content.is_empty() {
                let tool_info_hash = crate::render_cache::compute_tool_info_hash(&msg.tools);
                if let Some((cached, _tool_infos)) = cache.get_message(idx, &msg.content, tool_info_hash) {
                    for line in cached {
                        lines.push(line.clone());
                    }
                }
            }
        }
    });

    let speedup = no_cache_time.as_nanos() as f64 / with_cache_time.as_nanos() as f64;
    println!("  Cache speedup: {:.1}x faster", speedup);
}

/// Benchmark full transcript rendering simulation.
#[test]
fn bench_transcript_render_simulation() {
    println!("\n=== Full Transcript Render Simulation ===");

    let theme = Theme::dark();
    let width = 100usize;

    // Small transcript (10 messages)
    let small = build_transcript(10);
    bench("small transcript (10 msgs)", 50, || {
        let _ = render_transcript_sim(&small, &theme, width);
    });

    // Medium transcript (50 messages)
    let medium = build_transcript(50);
    bench("medium transcript (50 msgs)", 10, || {
        let _ = render_transcript_sim(&medium, &theme, width);
    });

    // Large transcript (200 messages)
    let large = build_transcript(200);
    bench("large transcript (200 msgs)", 3, || {
        let _ = render_transcript_sim(&large, &theme, width);
    });
}

/// Simulate transcript rendering (without actual ratatui Frame).
fn render_transcript_sim(transcript: &Transcript, theme: &Theme, width: usize) -> Vec<Line<'static>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;

    let mut lines: Vec<Line<'static>> = Vec::new();

    for msg in transcript.messages() {
        // Role line
        let (role_style, prefix) = match msg.role.as_str() {
            "user" => (
                Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
                "▸ ",
            ),
            "assistant" => (
                Style::default()
                    .fg(theme.assistant)
                    .add_modifier(Modifier::BOLD),
                "◂ ",
            ),
            _ => (Style::default().fg(theme.muted), "  "),
        };

        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, msg.role),
            role_style,
        )));

        // Content
        if msg.role == "assistant" && !msg.content.is_empty() {
            let md_width = width.saturating_sub(2);
            let md_lines = crate::markdown::render(&msg.content, theme, md_width);
            for line in md_lines {
                let mut indented = vec![Span::raw("  ")];
                indented.extend(line.spans);
                lines.push(Line::from(indented));
            }
        } else {
            for content_line in msg.content.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", content_line),
                    Style::default().fg(theme.fg),
                )));
            }
        }

        lines.push(Line::from("")); // spacing
    }

    lines
}

/// Benchmark with cache: simulates real-world scenario where most messages are cached.
#[test]
fn bench_cached_transcript_render() {
    println!("\n=== Cached Transcript Render (Real-world Scenario) ===");

    let theme = Theme::dark();
    let width = 100usize;
    let transcript = build_transcript(50);

    // Pre-populate cache (simulates messages that were rendered in previous frames)
    let mut cache = RenderCache::new();
    for (idx, msg) in transcript.messages().iter().enumerate() {
        if msg.role == "assistant" && !msg.content.is_empty() {
            let md_lines = crate::markdown::render(&msg.content, &theme, width);
            let lines: Vec<Line<'static>> = md_lines.into_iter().collect();
            let tool_info_hash = crate::render_cache::compute_tool_info_hash(&msg.tools);
            cache.set_message(idx, &msg.content, tool_info_hash, lines, vec![]);
        }
    }

    // Now benchmark rendering with cache hits
    bench("cached render (50 msgs, 100% hit)", 100, || {
        let _ = render_transcript_with_cache(&transcript, &cache, &theme, width);
    });

    // Compare with uncached
    bench("uncached render (50 msgs)", 5, || {
        let _ = render_transcript_sim(&transcript, &theme, width);
    });
}

/// Render with cache lookup.
fn render_transcript_with_cache(
    transcript: &Transcript,
    cache: &RenderCache,
    theme: &Theme,
    width: usize,
) -> Vec<Line<'static>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::Span;

    let mut lines: Vec<Line<'static>> = Vec::new();

    for (idx, msg) in transcript.messages().iter().enumerate() {
        // Try cache first for assistant messages
        if msg.role == "assistant" && !msg.content.is_empty() {
            let tool_info_hash = crate::render_cache::compute_tool_info_hash(&msg.tools);
            if let Some((cached, _tool_infos)) = cache.get_message(idx, &msg.content, tool_info_hash) {
                // Role line
                lines.push(Line::from(Span::styled(
                    format!("◂ {}", msg.role),
                    Style::default()
                        .fg(theme.assistant)
                        .add_modifier(Modifier::BOLD),
                )));
                // Cached content
                for line in cached {
                    lines.push(line.clone());
                }
                lines.push(Line::from("")); // spacing
                continue;
            }
        }

        // Fallback: render normally
        let (role_style, prefix) = match msg.role.as_str() {
            "user" => (
                Style::default().fg(theme.user).add_modifier(Modifier::BOLD),
                "▸ ",
            ),
            "assistant" => (
                Style::default()
                    .fg(theme.assistant)
                    .add_modifier(Modifier::BOLD),
                "◂ ",
            ),
            _ => (Style::default().fg(theme.muted), "  "),
        };

        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, msg.role),
            role_style,
        )));

        if msg.role == "assistant" && !msg.content.is_empty() {
            let md_width = width.saturating_sub(2);
            let md_lines = crate::markdown::render(&msg.content, theme, md_width);
            for line in md_lines {
                let mut indented = vec![Span::raw("  ")];
                indented.extend(line.spans);
                lines.push(Line::from(indented));
            }
        } else {
            for content_line in msg.content.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {}", content_line),
                    Style::default().fg(theme.fg),
                )));
            }
        }

        lines.push(Line::from("")); // spacing
    }

    lines
}

/// Summary benchmark: measures overall improvement.
#[test]
fn bench_summary() {
    println!("\n");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║              TUI RENDERING PERFORMANCE BENCHMARKS            ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // Run all benchmarks
    bench_markdown_parsing();
    bench_syntax_highlighting();
    bench_render_cache();
    bench_transcript_render_simulation();
    bench_cached_transcript_render();

    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("Key findings:");
    println!("  • Markdown parsing is the most expensive operation");
    println!("  • Syntax highlighting has high initial cost (lazy-loaded)");
    println!("  • Cache provides significant speedup for repeated renders");
    println!("  • Large transcripts benefit most from caching");
    println!("═══════════════════════════════════════════════════════════════");
}
