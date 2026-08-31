//! Model selection - extracted from app.rs for better separation of concerns.
//!
//! Manages available models, selection, and persistence.

use crate::client::{Client, ClientError};

/// Model entry for selection overlay.
#[derive(Debug, Clone)]
pub struct ModelEntry {
    pub provider: String,
    pub id: String,
    pub api_id: Option<String>,
    pub is_default: bool,
}

impl ModelEntry {
    /// Full qualified name (provider:api_id or provider:id).
    pub fn full_name(&self) -> String {
        let model_name = self.api_id.as_deref().unwrap_or(&self.id);
        format!("{}:{}", self.provider, model_name)
    }

    /// Display name for model selector - shows a readable name.
    /// Extracts the model family/variant from the full ID.
    /// e.g., "anthropic::2024-10-22::claude-haiku-4-5-latest" -> "claude-haiku-4-5-latest"
    pub fn display_name(&self) -> String {
        let model_name = self.api_id.as_deref().unwrap_or(&self.id);
        // If it contains "::", take the last segment (the actual model name).
        if let Some(pos) = model_name.rfind("::") {
            model_name[pos + 2..].to_string()
        } else {
            model_name.to_string()
        }
    }

    /// Short name for status bar (just the model identifier).
    pub fn short_name(&self) -> String {
        self.api_id.as_deref().unwrap_or(&self.id).to_string()
    }
}

/// Manages model list and selection.
#[derive(Debug, Clone)]
pub struct ModelSelector {
    /// Available models from connected providers.
    models: Vec<ModelEntry>,
    /// Currently selected model index.
    selected: usize,
}

impl ModelSelector {
    pub fn new() -> Self {
        Self {
            models: Vec::new(),
            selected: 0,
        }
    }

    /// Load available models from server.
    pub fn load_models(&mut self, client: &Client) -> Result<(), ClientError> {
        if let Ok(models) = client.list_models() {
            self.models = models
                .iter()
                .map(|m| ModelEntry {
                    provider: m.provider.clone(),
                    id: m.id.clone(),
                    api_id: m.api_id.clone(),
                    is_default: m.is_default,
                })
                .collect();

            // Try to restore last used model from server preferences.
            let last_model = client.get_pref("last_model");
            if let Some(ref model_id) = last_model {
                if let Some(idx) = self.models.iter().position(|m| &m.id == model_id) {
                    self.selected = idx;
                } else if let Some(idx) = self.models.iter().position(|m| m.is_default) {
                    self.selected = idx;
                }
            } else if let Some(idx) = self.models.iter().position(|m| m.is_default) {
                self.selected = idx;
            }
        }
        Ok(())
    }

    /// Get the current model display name.
    pub fn current_model_name(&self) -> String {
        self.models
            .get(self.selected)
            .map(|m| m.display_name())
            .unwrap_or_else(|| "no model".into())
    }

    /// Get the currently selected model.
    pub fn current_model(&self) -> Option<&ModelEntry> {
        self.models.get(self.selected)
    }

    /// Get model by index.
    pub fn get_model(&self, idx: usize) -> Option<&ModelEntry> {
        self.models.get(idx)
    }

    /// Get all models.
    pub fn models(&self) -> &[ModelEntry] {
        &self.models
    }

    /// Get model count.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// Get selected index.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Set selected index.
    pub fn set_selected(&mut self, idx: usize) {
        if idx < self.models.len() {
            self.selected = idx;
        }
    }

    /// Select previous model (wraps around).
    pub fn select_prev(&mut self) {
        if self.models.is_empty() {
            return;
        }
        if self.selected > 0 {
            self.selected -= 1;
        } else {
            self.selected = self.models.len() - 1;
        }
    }

    /// Select next model (wraps around).
    pub fn select_next(&mut self) {
        if self.models.is_empty() {
            return;
        }
        if self.selected < self.models.len() - 1 {
            self.selected += 1;
        } else {
            self.selected = 0;
        }
    }

    /// Apply selection and persist to server.
    ///
    /// Returns the selected model if successful.
    pub fn apply_selection(
        &mut self,
        idx: usize,
        client: &Client,
        session_id: &str,
        lease: &str,
    ) -> Result<Option<&ModelEntry>, ClientError> {
        if idx >= self.models.len() {
            return Ok(None);
        }

        self.selected = idx;
        let model = &self.models[idx];

        // Persist selection to server preferences.
        let _ = client.set_pref("last_model", &model.id);

        // Send model change to current session.
        if !session_id.is_empty() {
            client.set_model(session_id, lease, &model.provider, &model.id)?;
        }

        Ok(Some(&self.models[self.selected]))
    }

    /// Set initial model for a session (after acquiring lease).
    pub fn set_initial_model(
        &self,
        client: &Client,
        session_id: &str,
        lease: &str,
    ) -> Result<(), ClientError> {
        if let Some(model) = self.current_model() {
            client.set_model(session_id, lease, &model.provider, &model.id)?;
        }
        Ok(())
    }
}

impl Default for ModelSelector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_models() -> Vec<ModelEntry> {
        vec![
            ModelEntry {
                provider: "openai".into(),
                id: "gpt-4".into(),
                api_id: Some("gpt-4-turbo".into()),
                is_default: true,
            },
            ModelEntry {
                provider: "anthropic".into(),
                id: "claude-3".into(),
                api_id: Some("claude-3-opus".into()),
                is_default: false,
            },
            ModelEntry {
                provider: "anthropic".into(),
                id: "claude-3-sonnet".into(),
                api_id: None,
                is_default: false,
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
}
