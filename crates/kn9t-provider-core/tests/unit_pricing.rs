use kn9t_provider_core::pricing::lookup_price;

#[test]
fn test_claude_haiku_4() {
    let p = lookup_price("us.anthropic.claude-haiku-4-5-20251001-v1:0").unwrap();
    assert_eq!(p.input, 1_000_000);
    assert_eq!(p.output, 5_000_000);
}

#[test]
fn test_claude_sonnet_4() {
    let p = lookup_price("us.anthropic.claude-sonnet-4-5-20250929-v1:0").unwrap();
    assert_eq!(p.input, 3_000_000);
}

#[test]
fn test_nemotron_nano() {
    let p = lookup_price("nvidia.nemotron-nano-12b-v2").unwrap();
    assert_eq!(p.input, 150_000);
}

#[test]
fn test_nova_micro() {
    let p = lookup_price("amazon.nova-micro-v1:0").unwrap();
    assert_eq!(p.input, 35_000);
}

#[test]
fn test_gpt4o() {
    let p = lookup_price("gpt-4o-2024-08-06").unwrap();
    assert_eq!(p.input, 2_500_000);
}

#[test]
fn test_gpt4o_mini() {
    let p = lookup_price("gpt-4o-mini").unwrap();
    assert_eq!(p.input, 150_000);
}

#[test]
fn test_unknown() {
    assert!(lookup_price("some-random-model-xyz").is_none());
}

#[test]
fn test_table_loads() {
    // pricing_table is private; exercise it indirectly via a known match
    let p = lookup_price("gpt-4o-mini");
    assert!(p.is_some(), "pricing table should not be empty");
}

#[test]
fn test_custom_provider_claude_sonnet() {
    // custom provider plugin model ID format
    let p = lookup_price("anthropic::2024-10-22::claude-sonnet-4-5-thinking-latest").unwrap();
    assert_eq!(p.input, 3_000_000);
    assert_eq!(p.output, 15_000_000);
}

#[test]
fn test_custom_provider_claude_opus() {
    let p = lookup_price("anthropic::2024-10-22::claude-opus-4-5-latest").unwrap();
    assert_eq!(p.input, 5_000_000);
    assert_eq!(p.output, 25_000_000);
}

#[test]
fn test_custom_provider_claude_haiku() {
    let p = lookup_price("anthropic::2024-10-22::claude-haiku-4-5-latest").unwrap();
    assert_eq!(p.input, 1_000_000);
    assert_eq!(p.output, 5_000_000);
}
