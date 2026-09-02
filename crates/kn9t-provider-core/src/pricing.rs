//! Default pricing fallback for known models.
//!
//! Prices are loaded from `data/models.toml` at compile time.
//! Config-defined prices always override these defaults.

use kn9t_core::Price;

/// Model price entry from TOML.
#[derive(Clone)]
struct PriceEntry {
    pattern: regex::Regex,
    price: Price,
}

/// Compiled pricing table (lazy-initialized).
fn pricing_table() -> &'static [PriceEntry] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<Vec<PriceEntry>> = OnceLock::new();
    
    TABLE.get_or_init(|| {
        let toml_str = include_str!("../data/models.toml");
        parse_pricing_toml(toml_str)
    })
}

/// Parse the TOML pricing file into a list of pattern-price entries.
fn parse_pricing_toml(toml_str: &str) -> Vec<PriceEntry> {
    #[derive(serde::Deserialize)]
    struct TomlModel {
        pattern: String,
        input: f64,
        output: f64,
        #[serde(default)]
        cache_read: f64,
        #[serde(default)]
        cache_write: f64,
    }
    
    #[derive(serde::Deserialize)]
    struct TomlFile {
        model: Vec<TomlModel>,
    }
    
    let parsed: TomlFile = match toml::from_str(toml_str) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[kn9t-pricing] failed to parse models.toml: {e}");
            return Vec::new();
        }
    };
    
    parsed.model
        .into_iter()
        .filter_map(|m| {
            let pattern = match regex::RegexBuilder::new(&m.pattern)
                .case_insensitive(true)
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[kn9t-pricing] invalid pattern {:?}: {e}", m.pattern);
                    return None;
                }
            };
            Some(PriceEntry {
                pattern,
                price: Price {
                    input: (m.input * 1_000_000.0).round() as i64,
                    output: (m.output * 1_000_000.0).round() as i64,
                    cache_read: (m.cache_read * 1_000_000.0).round() as i64,
                    cache_write: (m.cache_write * 1_000_000.0).round() as i64,
                },
            })
        })
        .collect()
}

/// Lookup fallback price for a model by matching its api_id against known patterns.
/// Returns None if no match found (caller should use zero or config price).
pub fn lookup_price(api_id: &str) -> Option<Price> {
    let table = pricing_table();
    
    for entry in table {
        if entry.pattern.is_match(api_id) {
            return Some(entry.price.clone());
        }
    }
    
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let table = pricing_table();
        assert!(!table.is_empty(), "pricing table should not be empty");
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
}
