use kn9t_tui::app::App;
use kn9t_tui::config::Config;
use ratatui::layout::Rect;

fn app() -> App {
    App::new(Config::default(), kn9t_tui::event::TickControl::dummy())
}

#[test]
fn clicks_outside_transcript_are_ignored() {
    let mut a = app();
    a.transcript_area = Some(Rect::new(0, 0, 80, 10));

    assert!(!a.is_outside_transcript(5), "row inside transcript");
    assert!(a.is_outside_transcript(15), "row below transcript");
}

#[test]
fn unknown_geometry_does_not_block_clicks() {
    // Before the first frame nothing is recorded; clicks must still work.
    let a = app();
    assert!(!a.is_outside_transcript(10));
}
