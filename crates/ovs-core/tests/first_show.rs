//! A server that has never had a show made for it makes one.
//!
//! The owner, 2026-09-27: *"Kannst du noch machen, dass der Server immer eine
//! Leere Default Show hat? Es muss kein Ausgang gesetzt sein oder sonst was,
//! aber die sollte beim ersten Serverstart geladen sein."*
//!
//! A fresh install came up with no show at all: outputs black, the client's
//! list empty, and the first thing anybody had to work out was that a show is a
//! thing you create before anything else does anything.
//!
//! The rule that matters here is the second half of it — **one show, and never
//! a second** — which is obvious until somebody boots a server twice.

use ovs_core::server::startup::{ensure_a_show_exists, FIRST_SHOW};
use ovs_core::AppState;
use ovs_db::Database;
use ovs_types::config::Show;

fn server() -> AppState {
    AppState::new(Database::open_in_memory().expect("opens"), "0.0.1-test")
}

#[test]
fn a_server_with_no_show_makes_one() {
    let state = server();
    assert!(
        state
            .with_database(|database| database.list_shows())
            .expect("lists")
            .is_empty(),
        "a fresh database starts with nothing in it"
    );

    let made = ensure_a_show_exists(&state).expect("makes one");
    assert!(made.is_some(), "a server with no show makes one");

    let shows = state
        .with_database(|database| database.list_shows())
        .expect("lists");
    assert_eq!(shows.len(), 1, "exactly one");
    assert_eq!(shows[0].name, FIRST_SHOW);
    assert_eq!(Some(shows[0].id), made, "and it is the one just made");
}

#[test]
fn the_show_it_makes_is_empty() {
    // "Es muss kein Ausgang gesetzt sein oder sonst was." A server that
    // invented a layer or an output here would be guessing at a wall it has
    // never seen, and the operator would have to undo the guess before doing
    // anything.
    let state = server();
    let id = ensure_a_show_exists(&state)
        .expect("makes one")
        .expect("made");

    let show = state
        .with_database(|database| database.load_show(id))
        .expect("loads");
    assert!(show.layers.is_empty(), "no layers");
    assert!(show.outputs.is_empty(), "no outputs");
    assert!(show.pieces.is_empty(), "no pieces");
    assert!(show.fixture_patch.is_empty(), "nothing patched");
    // And a real document otherwise: openable by this build, which is what
    // makes it loadable rather than a row the client has to refuse.
    assert_eq!(show.schema_version, Show::new("x").schema_version);
}

#[test]
fn a_second_start_does_not_make_a_second_show() {
    // The one that goes wrong quietly: a server that made a show every time it
    // booted would hand an operator a list of forty identical shows over a
    // season, and the one they were working in would be somewhere in it.
    let state = server();
    let first = ensure_a_show_exists(&state).expect("makes one");
    assert!(first.is_some());

    for _ in 0..3 {
        assert_eq!(
            ensure_a_show_exists(&state).expect("looks"),
            None,
            "a server that has a show is not given another"
        );
    }

    let shows = state
        .with_database(|database| database.list_shows())
        .expect("lists");
    assert_eq!(shows.len(), 1, "still exactly one");
}

#[test]
fn a_server_that_already_has_shows_is_left_alone() {
    // Not even when the shows are somebody else's: an operator who has been
    // working on this server for a season does not want a "New Show" appearing
    // at the top of their list after an upgrade.
    let state = server();
    let theirs = Show::new("Werkhalle");
    let id = theirs.id;
    state
        .with_database(|database| database.save_show(&theirs))
        .expect("saves");

    assert_eq!(ensure_a_show_exists(&state).expect("looks"), None);

    let shows = state
        .with_database(|database| database.list_shows())
        .expect("lists");
    assert_eq!(shows.len(), 1);
    assert_eq!(shows[0].id, id, "theirs, untouched");
}
