//! 96E-13 regression: store docs must accurately describe the single-connection serialized model.

#[test]
fn p1_96e13_doc_clarifies_serialized_model() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let db_rs = manifest.join("src/db.rs");
    let txt = std::fs::read_to_string(&db_rs).expect("read db.rs");
    // Must NOT contain the old misleading WAL concurrent-readers claim without qualification
    let misleading = "WAL allows concurrent readers on separate connections";
    assert!(
        !txt.contains(misleading),
        "db.rs still contains misleading concurrency claim '{}'; 96E-13 requires it to be clarified or removed",
        misleading
    );
    // Must contain clarification of intentional single-connection serialized model
    // Check for key phrases indicating intentional serialization and WAL purpose
    let has_single = txt.contains("single") && txt.to_lowercase().contains("mutex") && txt.to_lowercase().contains("serialized");
    let has_wal_clarify = txt.to_lowercase().contains("wal") && (txt.to_lowercase().contains("crash") || txt.to_lowercase().contains("not for") || txt.to_lowercase().contains("serialized"));
    assert!(
        has_single,
        "db.rs must document intentional single-connection serialized model (expected 'single' + 'Mutex' + 'serialized')"
    );
    assert!(
        has_wal_clarify,
        "db.rs must clarify WAL purpose (crash safety, not in-process concurrency)"
    );
}
