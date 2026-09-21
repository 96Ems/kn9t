//! Unit tests for Bus and EventSink: broadcast and subscription.

use kn9t_core::{Bus, Event, EventSink, LiveEvent, Message, MsgId, Role};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn test_event() -> Event {
    Event::Error {
        message: "test".into(),
    }
}

fn test_live_event() -> LiveEvent {
    LiveEvent::Error {
        message: "test".into(),
    }
}

#[test]
fn test_bus_new() {
    let bus = Bus::new();
    // Should not panic
    drop(bus);
}

#[test]
fn test_bus_subscribe() {
    let bus = Bus::new();
    let _sub = bus.subscribe(10);
    // Subscription created successfully
}

#[test]
fn test_bus_publish_to_subscriber() {
    let bus = Bus::new();
    let sub = bus.subscribe(10);

    bus.publish(test_event());

    let event = sub.try_recv();
    assert!(event.is_some());
}

#[test]
fn test_bus_try_recv_empty() {
    let bus = Bus::new();
    let sub = bus.subscribe(10);

    let event = sub.try_recv();
    assert!(event.is_none());
}

#[test]
fn test_bus_multiple_subscribers() {
    let bus = Bus::new();
    let sub1 = bus.subscribe(10);
    let sub2 = bus.subscribe(10);

    bus.publish(test_event());

    assert!(sub1.try_recv().is_some());
    assert!(sub2.try_recv().is_some());
}

#[test]
fn test_bus_dropped_subscriber_pruned() {
    let bus = Bus::new();
    let sub1 = bus.subscribe(10);
    {
        let _sub2 = bus.subscribe(10);
        // sub2 dropped here
    }

    bus.publish(test_event());

    // sub1 should still work
    assert!(sub1.try_recv().is_some());
}

#[test]
fn test_bus_capacity_drops_oldest() {
    let bus = Bus::new();
    let sub = bus.subscribe(2); // Capacity of 2

    bus.publish(Event::Error {
        message: "first".into(),
    });
    bus.publish(Event::Error {
        message: "second".into(),
    });
    bus.publish(Event::Error {
        message: "third".into(),
    }); // Should drop "first"

    // Should receive "second" and "third", not "first"
    let e1 = sub.try_recv().unwrap();
    let e2 = sub.try_recv().unwrap();
    let e3 = sub.try_recv();

    match e1 {
        Event::Error { message } => assert_eq!(message, "second"),
        _ => panic!("expected Error event"),
    }
    match e2 {
        Event::Error { message } => assert_eq!(message, "third"),
        _ => panic!("expected Error event"),
    }
    assert!(e3.is_none());
}

#[test]
fn test_bus_recv_timeout_returns_none_on_timeout() {
    let bus = Bus::new();
    let sub = bus.subscribe(10);

    let result = sub.recv_timeout(Duration::from_millis(10));

    // Should return None because no event was published
    assert!(result.is_none());
}

#[test]
fn test_bus_recv_timeout_returns_event() {
    let bus = Bus::new();
    let sub = bus.subscribe(10);

    bus.publish(test_event());

    let result = sub.recv_timeout(Duration::from_secs(1));
    assert!(result.is_some());
}

#[test]
fn test_bus_recv_blocks_until_event() {
    let bus = Arc::new(Bus::new());
    let sub = bus.subscribe(10);
    let bus_clone = bus.clone();

    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        bus_clone.publish(test_event());
    });

    let start = std::time::Instant::now();
    let result = sub.recv();
    let elapsed = start.elapsed();

    assert!(result.is_some());
    assert!(elapsed >= Duration::from_millis(20));

    handle.join().unwrap();
}

#[test]
fn test_bus_drop_closes_subscriptions() {
    let sub;
    {
        let bus = Bus::new();
        sub = bus.subscribe(10);
        // bus dropped here
    }

    // recv should return None because bus is closed
    let result = sub.try_recv();
    assert!(result.is_none());

    // recv with timeout should also return None
    let result = sub.recv_timeout(Duration::from_millis(10));
    assert!(result.is_none());
}

#[test]
fn test_event_sink_trait() {
    let bus = Bus::new();
    let sub = bus.subscribe(10);

    // Use the EventSink trait — must be LiveEvent, not Event
    let sink: &dyn EventSink = &bus;
    sink.emit(test_live_event());

    assert!(sub.try_recv().is_some());
}

#[test]
fn test_event_sink_cannot_accept_durable() {
    // Compile-time guarantee: EventSink::emit takes LiveEvent, so the following
    // would not compile after
    //   let sink: &dyn EventSink = &Bus::new();
    //   sink.emit(Event::MessageAppended { seq: 0, msg: ... });
    // This test documents the type safety by asserting that LiveEvent does not
    // have durable variants and that Event::MessageAppended cannot be used as LiveEvent.
    // We verify at runtime that LiveEvent has no `seq` field and that a durable
    // Event is distinguishable.
    let live = LiveEvent::Error {
        message: "x".into(),
    };
    let event: Event = live.clone().into();
    assert!(
        event.seq().is_none(),
        "LiveEvent must be transient (no seq)"
    );
    let durable = Event::MessageAppended {
        seq: 1,
        msg: Message {
            id: MsgId::new(),
            role: Role::User,
            content: vec![],
            silent: false,
        },
    };
    assert!(durable.seq().is_some(), "durable must have seq");
    // The type system prevents `sink.emit(durable)` — this would be a compile error:
    // `expected LiveEvent, found Event`
}

#[test]
fn test_bus_default() {
    let bus = Bus::default();
    let sub = bus.subscribe(10);
    bus.publish(test_event());
    assert!(sub.try_recv().is_some());
}
