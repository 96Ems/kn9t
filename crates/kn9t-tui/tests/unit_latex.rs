use kn9t_tui::latex::{latex_to_unicode, process_math};

#[test]
fn test_greek_letters() {
    assert_eq!(latex_to_unicode(r"\alpha"), "α");
    assert_eq!(latex_to_unicode(r"\beta"), "β");
    assert_eq!(latex_to_unicode(r"\Gamma"), "Γ");
    assert_eq!(latex_to_unicode(r"\pi"), "π");
}

#[test]
fn test_operators() {
    assert_eq!(latex_to_unicode(r"\sum"), "∑");
    assert_eq!(latex_to_unicode(r"\int"), "∫");
    assert_eq!(latex_to_unicode(r"\infty"), "∞");
    assert_eq!(latex_to_unicode(r"\partial"), "∂");
}

#[test]
fn test_relations() {
    assert_eq!(latex_to_unicode(r"\leq"), "≤");
    assert_eq!(latex_to_unicode(r"\geq"), "≥");
    assert_eq!(latex_to_unicode(r"\neq"), "≠");
    assert_eq!(latex_to_unicode(r"\approx"), "≈");
}

#[test]
fn test_subscript() {
    assert_eq!(latex_to_unicode("x_2"), "x₂");
    assert_eq!(latex_to_unicode("a_{12}"), "a₁₂");
}

#[test]
fn test_superscript() {
    assert_eq!(latex_to_unicode("x^2"), "x²");
    assert_eq!(latex_to_unicode("e^{i\\pi}"), "eⁱπ");
}

#[test]
fn test_frac() {
    assert_eq!(latex_to_unicode(r"\frac{a}{b}"), "a/b");
    assert_eq!(latex_to_unicode(r"\frac{1}{2}"), "1/2");
}

#[test]
fn test_sqrt() {
    assert_eq!(latex_to_unicode(r"\sqrt{x}"), "√x");
    assert_eq!(latex_to_unicode(r"\sqrt[3]{x}"), "³√x"); // cube root: ³√x
}

#[test]
fn test_complex_expression() {
    let expr = r"E = mc^2";
    assert_eq!(latex_to_unicode(expr), "E = mc²");

    let quadratic = r"x = \frac{-b \pm \sqrt{b^2 - 4ac}}{2a}";
    let result = latex_to_unicode(quadratic);
    assert!(result.contains("±"));
    assert!(result.contains("√"));
    assert!(result.contains("²"));
}

#[test]
fn test_inline_math() {
    let text = "The equation $E = mc^2$ is famous.";
    let result = process_math(text);
    assert_eq!(result, "The equation E = mc² is famous.");
}

#[test]
fn test_display_math() {
    let text = "Here is a formula: $$x^2 + y^2 = r^2$$";
    let result = process_math(text);
    assert!(result.contains("x² + y² = r²"));
}
