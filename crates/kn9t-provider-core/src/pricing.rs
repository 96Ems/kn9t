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

    parsed
        .model
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
            return Some(entry.price);
        }
    }

    None
}

