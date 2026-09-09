use kn9t_plugin_sdk::SseReader;

#[test]
fn parse_two_events() {
    let raw = b"event: completion\ndata: {\"deltaText\":\"hel\"}\n\n\
                event: completion\ndata: {\"deltaText\":\"lo\"}\n\n\
                event: done\ndata: {}\n\n";
    let mut r = SseReader::new(raw.as_slice());
    let e1 = r.next().unwrap().unwrap();
    assert_eq!(e1.event, "completion");
    assert!(e1.data.contains("hel"));
    let e2 = r.next().unwrap().unwrap();
    assert_eq!(e2.event, "completion");
    assert!(e2.data.contains("lo"));
    let e3 = r.next().unwrap().unwrap();
    assert_eq!(e3.event, "done");
}

#[test]
fn split_across_chunks() {
    // Simulate a block split across two lines of the same field (multi-data lines).
    let raw = b"event: completion\ndata: {\"a\":1}\ndata: {\"b\":2}\n\n";
    let mut r = SseReader::new(raw.as_slice());
    let e = r.next().unwrap().unwrap();
    // data_buf = "{\"a\":1}\n{\"b\":2}" — both lines present
    assert!(e.data.contains("a"));
    assert!(e.data.contains("b"));
}
