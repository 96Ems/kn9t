//! LaTeX to Unicode approximation for terminal rendering.
//!
//! Converts common LaTeX math notation to Unicode symbols.
//! This is a best-effort approximation - complex expressions may not render perfectly.

use std::collections::HashMap;
use std::sync::LazyLock;

/// Greek letters mapping
static GREEK: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        // Lowercase
        ("alpha", "α"),
        ("beta", "β"),
        ("gamma", "γ"),
        ("delta", "δ"),
        ("epsilon", "ε"),
        ("varepsilon", "ε"),
        ("zeta", "ζ"),
        ("eta", "η"),
        ("theta", "θ"),
        ("vartheta", "ϑ"),
        ("iota", "ι"),
        ("kappa", "κ"),
        ("lambda", "λ"),
        ("mu", "μ"),
        ("nu", "ν"),
        ("xi", "ξ"),
        ("pi", "π"),
        ("varpi", "ϖ"),
        ("rho", "ρ"),
        ("varrho", "ϱ"),
        ("sigma", "σ"),
        ("varsigma", "ς"),
        ("tau", "τ"),
        ("upsilon", "υ"),
        ("phi", "φ"),
        ("varphi", "ϕ"),
        ("chi", "χ"),
        ("psi", "ψ"),
        ("omega", "ω"),
        // Uppercase
        ("Gamma", "Γ"),
        ("Delta", "Δ"),
        ("Theta", "Θ"),
        ("Lambda", "Λ"),
        ("Xi", "Ξ"),
        ("Pi", "Π"),
        ("Sigma", "Σ"),
        ("Upsilon", "Υ"),
        ("Phi", "Φ"),
        ("Psi", "Ψ"),
        ("Omega", "Ω"),
    ])
});

/// Mathematical operators and symbols
static OPERATORS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        // Operators
        ("sum", "∑"),
        ("prod", "∏"),
        ("int", "∫"),
        ("oint", "∮"),
        ("iint", "∬"),
        ("iiint", "∭"),
        ("partial", "∂"),
        ("nabla", "∇"),
        ("infty", "∞"),
        ("pm", "±"),
        ("mp", "∓"),
        ("times", "×"),
        ("div", "÷"),
        ("cdot", "·"),
        ("ast", "∗"),
        ("star", "⋆"),
        ("cbrt", "∛"), // sqrt handled specially for \sqrt[n]{x}
        // Relations
        ("leq", "≤"),
        ("le", "≤"),
        ("geq", "≥"),
        ("ge", "≥"),
        ("neq", "≠"),
        ("ne", "≠"),
        ("approx", "≈"),
        ("equiv", "≡"),
        ("sim", "∼"),
        ("simeq", "≃"),
        ("cong", "≅"),
        ("propto", "∝"),
        ("ll", "≪"),
        ("gg", "≫"),
        ("subset", "⊂"),
        ("supset", "⊃"),
        ("subseteq", "⊆"),
        ("supseteq", "⊇"),
        ("in", "∈"),
        ("notin", "∉"),
        ("ni", "∋"),
        ("cup", "∪"),
        ("cap", "∩"),
        ("setminus", "∖"),
        ("emptyset", "∅"),
        ("varnothing", "∅"),
        // Arrows
        ("to", "→"),
        ("rightarrow", "→"),
        ("leftarrow", "←"),
        ("Rightarrow", "⇒"),
        ("Leftarrow", "⇐"),
        ("Leftrightarrow", "⇔"),
        ("leftrightarrow", "↔"),
        ("mapsto", "↦"),
        ("uparrow", "↑"),
        ("downarrow", "↓"),
        ("updownarrow", "↕"),
        // Logic
        ("forall", "∀"),
        ("exists", "∃"),
        ("nexists", "∄"),
        ("land", "∧"),
        ("lor", "∨"),
        ("lnot", "¬"),
        ("neg", "¬"),
        ("implies", "⟹"),
        ("iff", "⟺"),
        // Misc
        ("ldots", "…"),
        ("cdots", "⋯"),
        ("vdots", "⋮"),
        ("ddots", "⋱"),
        ("prime", "′"),
        ("degree", "°"),
        ("circ", "∘"),
        ("angle", "∠"),
        ("perp", "⊥"),
        ("parallel", "∥"),
        ("therefore", "∴"),
        ("because", "∵"),
        ("aleph", "ℵ"),
        ("hbar", "ℏ"),
        ("ell", "ℓ"),
        ("Re", "ℜ"),
        ("Im", "ℑ"),
        ("wp", "℘"),
    ])
});

/// Accents and modifiers
static ACCENTS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    HashMap::from([
        ("hat", "̂"),
        ("bar", "̄"),
        ("dot", "̇"),
        ("ddot", "̈"),
        ("vec", "⃗"),
        ("tilde", "̃"),
    ])
});

/// Font/style commands (just strip them, keep content)
static FONT_COMMANDS: &[&str] = &[
    "mathbf",
    "mathit",
    "mathrm",
    "mathsf",
    "mathtt",
    "mathcal",
    "mathfrak",
    "textbf",
    "textit",
    "textrm",
    "textsf",
    "texttt",
    "boldsymbol",
    "bm",
];

/// Convert a LaTeX math expression to Unicode approximation.
pub fn latex_to_unicode(latex: &str) -> String {
    let mut result = String::new();
    let mut chars = latex.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            // Parse command
            let mut cmd = String::new();
            while let Some(&next) = chars.peek() {
                if next.is_ascii_alphabetic() {
                    cmd.push(chars.next().unwrap());
                } else {
                    break;
                }
            }

            if cmd.is_empty() {
                // Escaped character like \{ \} \_ etc
                if let Some(escaped) = chars.next() {
                    result.push(escaped);
                }
            } else if let Some(&greek) = GREEK.get(cmd.as_str()) {
                result.push_str(greek);
            } else if let Some(&op) = OPERATORS.get(cmd.as_str()) {
                result.push_str(op);
            } else if FONT_COMMANDS.contains(&cmd.as_str()) {
                // Skip font commands, just include the content
                // Skip optional whitespace and opening brace
                while let Some(&next) = chars.peek() {
                    if next.is_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&'{') {
                    chars.next(); // skip {
                                  // Read until matching }
                    let mut depth = 1;
                    let mut content = String::new();
                    for c in chars.by_ref() {
                        if c == '{' {
                            depth += 1;
                            content.push(c);
                        } else if c == '}' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            content.push(c);
                        } else {
                            content.push(c);
                        }
                    }
                    result.push_str(&latex_to_unicode(&content));
                }
            } else if cmd == "frac" {
                // \frac{a}{b} -> a/b
                let num = read_braced(&mut chars);
                let den = read_braced(&mut chars);
                result.push_str(&latex_to_unicode(&num));
                result.push('/');
                result.push_str(&latex_to_unicode(&den));
            } else if cmd == "sqrt" {
                if chars.peek() == Some(&'[') {
                    // \sqrt[n]{x} -> ⁿ√x
                    chars.next(); // skip [
                    let mut n = String::new();
                    while let Some(&c) = chars.peek() {
                        if c == ']' {
                            chars.next();
                            break;
                        }
                        n.push(chars.next().unwrap());
                    }
                    result.push_str(&to_superscript(&n));
                }
                result.push('√');
                let content = read_braced(&mut chars);
                result.push_str(&latex_to_unicode(&content));
            } else if cmd == "text" || cmd == "textrm" || cmd == "mathrm" {
                let content = read_braced(&mut chars);
                result.push_str(&content);
            } else if let Some(&accent) = ACCENTS.get(cmd.as_str()) {
                let content = read_braced(&mut chars);
                result.push_str(&latex_to_unicode(&content));
                result.push_str(accent);
            } else {
                // Unknown command - keep as-is
                result.push('\\');
                result.push_str(&cmd);
            }
        } else if c == '_' {
            // Subscript
            let sub = read_next_group(&mut chars);
            result.push_str(&to_subscript(&latex_to_unicode(&sub)));
        } else if c == '^' {
            // Superscript
            let sup = read_next_group(&mut chars);
            result.push_str(&to_superscript(&latex_to_unicode(&sup)));
        } else if c == '{' || c == '}' {
            // Skip braces (they're for grouping)
        } else if c == '~' {
            result.push(' '); // Non-breaking space
        } else {
            result.push(c);
        }
    }

    result
}

/// Read a brace-delimited group like {content}
fn read_braced(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    // Skip whitespace
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }

    if chars.peek() != Some(&'{') {
        // Single character
        return chars.next().map(|c| c.to_string()).unwrap_or_default();
    }

    chars.next(); // skip {
    let mut content = String::new();
    let mut depth = 1;

    for c in chars.by_ref() {
        if c == '{' {
            depth += 1;
            content.push(c);
        } else if c == '}' {
            depth -= 1;
            if depth == 0 {
                break;
            }
            content.push(c);
        } else {
            content.push(c);
        }
    }

    content
}

/// Read next group (either {...} or single char)
fn read_next_group(chars: &mut std::iter::Peekable<std::str::Chars>) -> String {
    // Skip whitespace
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }

    if chars.peek() == Some(&'{') {
        read_braced(chars)
    } else {
        chars.next().map(|c| c.to_string()).unwrap_or_default()
    }
}

/// Convert string to Unicode subscript
fn to_subscript(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '0' => '₀',
            '1' => '₁',
            '2' => '₂',
            '3' => '₃',
            '4' => '₄',
            '5' => '₅',
            '6' => '₆',
            '7' => '₇',
            '8' => '₈',
            '9' => '₉',
            '+' => '₊',
            '-' => '₋',
            '=' => '₌',
            '(' => '₍',
            ')' => '₎',
            'a' => 'ₐ',
            'e' => 'ₑ',
            'h' => 'ₕ',
            'i' => 'ᵢ',
            'j' => 'ⱼ',
            'k' => 'ₖ',
            'l' => 'ₗ',
            'm' => 'ₘ',
            'n' => 'ₙ',
            'o' => 'ₒ',
            'p' => 'ₚ',
            'r' => 'ᵣ',
            's' => 'ₛ',
            't' => 'ₜ',
            'u' => 'ᵤ',
            'v' => 'ᵥ',
            'x' => 'ₓ',
            _ => c, // No subscript version available
        })
        .collect()
}

/// Convert string to Unicode superscript
fn to_superscript(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '0' => '⁰',
            '1' => '¹',
            '2' => '²',
            '3' => '³',
            '4' => '⁴',
            '5' => '⁵',
            '6' => '⁶',
            '7' => '⁷',
            '8' => '⁸',
            '9' => '⁹',
            '+' => '⁺',
            '-' => '⁻',
            '=' => '⁼',
            '(' => '⁽',
            ')' => '⁾',
            'a' => 'ᵃ',
            'b' => 'ᵇ',
            'c' => 'ᶜ',
            'd' => 'ᵈ',
            'e' => 'ᵉ',
            'f' => 'ᶠ',
            'g' => 'ᵍ',
            'h' => 'ʰ',
            'i' => 'ⁱ',
            'j' => 'ʲ',
            'k' => 'ᵏ',
            'l' => 'ˡ',
            'm' => 'ᵐ',
            'n' => 'ⁿ',
            'o' => 'ᵒ',
            'p' => 'ᵖ',
            'r' => 'ʳ',
            's' => 'ˢ',
            't' => 'ᵗ',
            'u' => 'ᵘ',
            'v' => 'ᵛ',
            'w' => 'ʷ',
            'x' => 'ˣ',
            'y' => 'ʸ',
            'z' => 'ᶻ',
            _ => c, // No superscript version available
        })
        .collect()
}

/// Process text containing inline math ($...$) and display math ($$...$$)
pub fn process_math(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '$' {
            let display = chars.peek() == Some(&'$');
            if display {
                chars.next(); // skip second $
            }

            // Collect math content
            let mut math = String::new();
            let mut found_end = false;

            while let Some(mc) = chars.next() {
                if mc == '$' {
                    if display {
                        if chars.peek() == Some(&'$') {
                            chars.next(); // skip second $
                            found_end = true;
                            break;
                        }
                        math.push(mc);
                    } else {
                        found_end = true;
                        break;
                    }
                } else {
                    math.push(mc);
                }
            }

            if found_end {
                let converted = latex_to_unicode(&math);
                if display {
                    result.push('\n');
                    result.push_str(&converted);
                    result.push('\n');
                } else {
                    result.push_str(&converted);
                }
            } else {
                // Unclosed math - output as-is
                result.push('$');
                if display {
                    result.push('$');
                }
                result.push_str(&math);
            }
        } else {
            result.push(c);
        }
    }

    result
}

