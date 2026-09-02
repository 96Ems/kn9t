//! 96E-14 regression: persisted cost must use integer micros, not f64.

#[test]
fn p1_96e14_price_and_cost_are_integer_micros() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let model_rs = manifest
        .ancestors()
        .find(|p| p.join("crates/kn9t-core/src/model.rs").exists())
        .map(|p| p.join("crates/kn9t-core/src/model.rs"))
        .unwrap();
    let event_rs = manifest
        .ancestors()
        .find(|p| p.join("crates/kn9t-core/src/event.rs").exists())
        .map(|p| p.join("crates/kn9t-core/src/event.rs"))
        .unwrap();
    let db_rs = manifest
        .ancestors()
        .find(|p| p.join("crates/kn9t-store/src/db.rs").exists())
        .map(|p| p.join("crates/kn9t-store/src/db.rs"))
        .unwrap();
    let project_rs = manifest
        .ancestors()
        .find(|p| p.join("crates/kn9t-store/src/project.rs").exists())
        .map(|p| p.join("crates/kn9t-store/src/project.rs"))
        .unwrap();

    let model_txt = std::fs::read_to_string(&model_rs).unwrap();
    let event_txt = std::fs::read_to_string(&event_rs).unwrap();
    let db_txt = std::fs::read_to_string(&db_rs).unwrap();
    let project_txt = std::fs::read_to_string(&project_rs).unwrap();

    // Price must be integer micros, not f64
    assert!(
        model_txt.contains("MoneyMicros") || model_txt.contains("cost_micros") || model_txt.contains("price_micros") || model_txt.contains("i64") && model_txt.contains("Price"),
        "model.rs Price should use integer micros (MoneyMicros/i64), still uses f64"
    );
    // Price must have integer micros field (new) — allow f64 to remain for compat during migration
    assert!(
        model_txt.contains("MoneyMicros") || model_txt.contains("cost_micros") || model_txt.contains("price_micros") || (model_txt.contains("i64") && model_txt.contains("Price")),
        "model.rs Price should have integer micros field"
    );
    assert!(
        event_txt.contains("cost_micros") || event_txt.contains("MoneyMicros"),
        "event.rs UsageRecorded should have cost_micros: i64/MoneMicros"
    );

    // DB schema must have cost_micros INTEGER (may keep cost_usd REAL for migration)
    let usage_ddl = db_txt.split("CREATE TABLE IF NOT EXISTS usage").nth(1).unwrap_or("");
    assert!(
        usage_ddl.contains("cost_micros") && usage_ddl.contains("INTEGER"),
        "usage table should have cost_micros INTEGER, got: {}",
        &usage_ddl[..3000.min(usage_ddl.len())]
    );

    // Project cost calculation must use integer micros
    assert!(
        project_txt.contains("cost_micros") || project_txt.contains("MoneyMicros"),
        "project.rs should compute cost_micros via integer arithmetic"
    );
}

#[test]
fn p1_96e14_rounding_boundary_deterministic() {
    use kn9t_core::{Price, Tokens, cost_micros};
    // Price section must be integer (checked above); now test deterministic calc
    let model_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|p| p.join("crates/kn9t-core/src/model.rs").exists())
        .unwrap()
        .join("crates/kn9t-core/src/model.rs");
    let txt = std::fs::read_to_string(model_path).unwrap();
    let price_section = txt.split("struct Price").nth(1).unwrap_or("");
    let price_def = price_section.split("}").next().unwrap_or("");
    if price_def.contains("f64") {
        panic!("96E-14 not yet fixed: Price still f64");
    }

    // Tricky float case: price 0.035 (35000 micros) with 1M tokens.
    // Float: 1_000_000 * 0.035 / 1e6 = 0.035, but binary float representation
    // of 0.035 is 0.034999..., sum of such costs can drift.
    // Integer: 1_000_000 * 35000 / 1_000_000 = 35000 exactly.
    let price = Price { input: 35_000, output: 0, cache_read: 0, cache_write: 0 };
    let tokens = Tokens { input: 1_000_000, output: 0, cache_read: 0, cache_write: 0, reasoning: 0 };
    let cost = cost_micros(&tokens, &price);
    assert_eq!(cost, 35_000, "1M tokens at $0.035 per 1M must be 35000 micros, got {}", cost);

    // Another boundary: 333333 tokens at $3 per 1M -> 999999 micros (not 1000000)
    // Float: 333333 * 3.0 / 1e6 = 0.999999, float may give 0.999999999 etc.
    // Integer: 333333 * 3_000_000 / 1_000_000 = 999999 exactly.
    let price2 = Price { input: 3_000_000, output: 0, cache_read: 0, cache_write: 0 };
    let tokens2 = Tokens { input: 333_333, output: 0, cache_read: 0, cache_write: 0, reasoning: 0 };
    let cost2 = cost_micros(&tokens2, &price2);
    assert_eq!(cost2, 999_999, "333333*3/1e6 must be 999999 micros");

    // Budget comparison must be integer, not float epsilon
    let budget_micros = 1_000_000; // $1.00
    assert!(cost2 < budget_micros, "999999 < 1000000 must hold deterministically");
    assert_eq!(budget_micros - cost2, 1, "remaining budget 1 micro");

    // Two identical costs must compare equal as integers, even where float would have epsilon
    let c1 = cost_micros(&tokens, &price);
    let c2 = cost_micros(&tokens, &price);
    assert_eq!(c1, c2, "identical inputs must give identical micros");
}
