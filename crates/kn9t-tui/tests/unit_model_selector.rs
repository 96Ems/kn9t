use kn9t_tui::model_selector::{ModelEntry, ModelSelector};

fn sample_models() -> Vec<ModelEntry> {
    vec![
        ModelEntry {
            provider: "openai".into(),
            id: "gpt-4".into(),
            api_id: Some("gpt-4-turbo".into()),
            is_default: true,
            ctx_window: None,
            max_out: None,
        },
        ModelEntry {
            provider: "anthropic".into(),
            id: "claude-3".into(),
            api_id: Some("claude-3-opus".into()),
            is_default: false,
            ctx_window: None,
            max_out: None,
        },
        ModelEntry {
            provider: "anthropic".into(),
            id: "claude-3-sonnet".into(),
            api_id: None,
            is_default: false,
            ctx_window: None,
            max_out: None,
        },
    ]
}

#[test]
fn test_model_entry_display_name() {
    let models = sample_models();

    // display_name extracts just the model name (no provider prefix).
    assert_eq!(models[0].display_name(), "gpt-4-turbo");
    assert_eq!(models[1].display_name(), "claude-3-opus");
    assert_eq!(models[2].display_name(), "claude-3-sonnet");
}

#[test]
fn test_model_entry_full_name() {
    let models = sample_models();

    // full_name includes provider prefix.
    assert_eq!(models[0].full_name(), "openai:gpt-4-turbo");
    assert_eq!(models[1].full_name(), "anthropic:claude-3-opus");
    assert_eq!(models[2].full_name(), "anthropic:claude-3-sonnet");
}

#[test]
fn test_model_entry_display_name_with_colons() {
    // Test model IDs that contain "::" (like Anthropic plugin format).
    let model = ModelEntry {
        provider: "custom-provider".into(),
        id: "anthropic".into(),
        api_id: Some("anthropic::2024-10-22::claude-haiku-4-5-latest".into()),
        is_default: false,
        ctx_window: None,
        max_out: None,
    };
    // Should extract just the last segment after "::".
    assert_eq!(model.display_name(), "claude-haiku-4-5-latest");
}

#[test]
fn test_model_entry_short_name() {
    let models = sample_models();

    assert_eq!(models[0].short_name(), "gpt-4-turbo");
    assert_eq!(models[1].short_name(), "claude-3-opus");
    assert_eq!(models[2].short_name(), "claude-3-sonnet");
}

#[test]
fn test_selector_navigation() {
    let mut selector = ModelSelector::new();
    selector.models = sample_models();
    selector.selected = 0;

    // Next.
    selector.select_next();
    assert_eq!(selector.selected(), 1);

    selector.select_next();
    assert_eq!(selector.selected(), 2);

    // Wrap around.
    selector.select_next();
    assert_eq!(selector.selected(), 0);

    // Prev wrap around.
    selector.select_prev();
    assert_eq!(selector.selected(), 2);

    selector.select_prev();
    assert_eq!(selector.selected(), 1);
}

#[test]
fn test_selector_current_model_name() {
    let mut selector = ModelSelector::new();
    selector.models = sample_models();
    selector.selected = 1;

    // display_name now returns just the model name, not provider:model.
    assert_eq!(selector.current_model_name(), "claude-3-opus");

    // Empty selector.
    let empty = ModelSelector::new();
    assert_eq!(empty.current_model_name(), "no model");
}

#[test]
fn test_selector_set_selected_bounds() {
    let mut selector = ModelSelector::new();
    selector.models = sample_models();
    selector.selected = 0;

    // Valid index.
    selector.set_selected(2);
    assert_eq!(selector.selected(), 2);

    // Invalid index - should not change.
    selector.set_selected(100);
    assert_eq!(selector.selected(), 2);
}
