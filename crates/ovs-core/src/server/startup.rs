//! What a server does for itself before it answers anything.

use ovs_db::DbError;
use ovs_types::config::Show;
use ovs_types::id::ShowId;

use super::AppState;

/// The name a server gives the show it makes for itself.
///
/// Plain, because it is a real show and the operator will rename it or replace
/// it. Not "Untitled": this one is meant to be used.
pub const FIRST_SHOW: &str = "New Show";

/// Makes sure this server has a show, and says which one it made, if it made one.
///
/// **A server that has never had a show made for it makes one** (owner,
/// 2026-09-27: "der Server [sollte] immer eine Leere Default Show [haben] …
/// beim ersten Serverstart geladen"). A fresh install had none, so it came up
/// with the outputs black and an empty list, and the first thing anybody had to
/// work out was that a show is a thing you create before anything else does
/// anything.
///
/// Empty in the sense the owner asked for — "Es muss kein Ausgang gesetzt sein
/// oder sonst was": whatever `Show::new` is, which is the same document the
/// Show tab's own create button produces. A server that invented a layout here
/// would be guessing at a wall it has never seen.
///
/// Only when there are none at all, so this is not a show a boot leaves behind.
/// An operator who has deleted every show has the same server a new one does,
/// and gets the same answer.
///
/// # Errors
///
/// Returns [`DbError`] if the list cannot be read or the show cannot be saved.
pub fn ensure_a_show_exists(state: &AppState) -> Result<Option<ShowId>, DbError> {
    state.with_database(|database| {
        if !database.list_shows()?.is_empty() {
            return Ok(None);
        }
        let show = Show::new(FIRST_SHOW);
        let id = show.id;
        database.save_show(&show)?;
        Ok(Some(id))
    })
}
