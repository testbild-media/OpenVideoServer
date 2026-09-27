//! Letting somebody watch one surface, for one minute, and nothing else.
//!
//! A session token is the keys to the building. It opens every route on the
//! API: it loads shows, it moves faders, it stops the machine. The desktop
//! client's window needs to prove it may watch a preview, and until now the only
//! thing it had to prove it with was that token — so a page that should only
//! have been able to see pictures was handed something that could end a show.
//!
//! A ticket is the narrow version. It is minted by a request that *did* carry a
//! session, it names one surface, it is good for a minute, and it is spent the
//! moment it is used. Everything else about it is deliberately missing: it
//! cannot be exchanged for a session, it cannot be used twice, and a ticket for
//! the composition will not open an output.
//!
//! # Why single use
//!
//! A watcher mints one per socket, so nothing is lost by it, and what is gained
//! is that a ticket read off a wire or out of a page's memory is worth nothing
//! by the time anybody could use it: the socket it was minted for has already
//! spent it.
//!
//! # Why a minute
//!
//! Long enough that a slow machine, a reconnect and a clock a few seconds out
//! all still work. Short enough that a ticket sitting in a log is not a way in
//! tomorrow morning.

use std::collections::HashMap;

use ovs_types::telemetry::StreamSurface;
use ovs_types::time::Timestamp;

use crate::auth::AuthError;

/// How long a ticket is good for.
pub const TICKET_LIFETIME_MS: i64 = 60_000;

/// The most tickets that may be outstanding at once.
///
/// A ticket costs nothing to mint and something to keep, so a client that mints
/// and never watches is a slow leak. This is far above what any real rig asks
/// for — a handful of operators reconnecting is single figures — and its only
/// job is to make the leak bounded.
const OUTSTANDING: usize = 512;

/// One minted ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Ticket {
    /// The only surface it opens.
    surface: StreamSurface,
    /// When it stops working.
    expires_at: Timestamp,
}

/// The tickets that have been minted and not yet spent.
#[derive(Debug, Default)]
pub struct StreamTickets {
    by_ticket: HashMap<String, Ticket>,
}

impl StreamTickets {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mints a ticket for one surface.
    ///
    /// The caller has already checked that whoever asked holds a session: this
    /// type mints, it does not decide. Keeping those apart means this cannot
    /// become the place a credential check is skipped.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Entropy`] if the system random source fails, which
    /// is worth failing loudly over rather than falling back to something
    /// weaker.
    pub fn mint(
        &mut self,
        surface: StreamSurface,
        now: Timestamp,
    ) -> Result<(String, Timestamp), AuthError> {
        // Expired tickets go whenever one is minted, so a store never grows
        // past what is actually in flight and nothing needs a sweeper thread.
        self.by_ticket
            .retain(|_, ticket| ticket.expires_at.as_millis() > now.as_millis());

        if self.by_ticket.len() >= OUTSTANDING {
            return Err(AuthError::InvalidToken);
        }

        let ticket = crate::auth::issue_opaque_token()?;
        let expires_at = Timestamp::from_millis(now.as_millis().saturating_add(TICKET_LIFETIME_MS));

        self.by_ticket.insert(
            ticket.clone(),
            Ticket {
                surface,
                expires_at,
            },
        );

        Ok((ticket, expires_at))
    }

    /// Spends a ticket, and says whether it was good for that surface.
    ///
    /// Removed whether or not the surface matched: a ticket presented to the
    /// wrong surface has been used, and letting it be tried against each
    /// surface in turn would make it a ticket for all of them.
    pub fn spend(&mut self, ticket: &str, surface: StreamSurface, now: Timestamp) -> bool {
        let Some(found) = self.by_ticket.remove(ticket) else {
            return false;
        };

        found.surface == surface && found.expires_at.as_millis() > now.as_millis()
    }

    /// How many tickets are outstanding.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_ticket.len()
    }

    /// Whether any ticket is outstanding.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_ticket.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_millis(1_700_000_000_000)
    }

    fn later(millis: i64) -> Timestamp {
        Timestamp::from_millis(now().as_millis().saturating_add(millis))
    }

    #[test]
    fn a_ticket_opens_the_surface_it_was_minted_for() {
        let mut tickets = StreamTickets::new();
        let (ticket, expires_at) = tickets
            .mint(StreamSurface::Composition, now())
            .expect("mints");

        assert_eq!(expires_at.as_millis(), now().as_millis() + 60_000);
        assert!(tickets.spend(&ticket, StreamSurface::Composition, now()));
    }

    #[test]
    fn a_ticket_for_one_surface_does_not_open_another() {
        // The whole point of a ticket: it is narrower than a session. One that
        // opened any surface would only be a session with a shorter life.
        let mut tickets = StreamTickets::new();
        let output_id = ovs_types::id::OutputId::new();
        let (ticket, _) = tickets
            .mint(StreamSurface::Composition, now())
            .expect("mints");

        assert!(!tickets.spend(&ticket, StreamSurface::Output { output_id }, now()));
    }

    #[test]
    fn a_ticket_is_spent_the_first_time_it_is_used() {
        // So that one read off a wire is worth nothing by the time anybody
        // could use it: the socket it was minted for has already spent it.
        let mut tickets = StreamTickets::new();
        let (ticket, _) = tickets
            .mint(StreamSurface::Composition, now())
            .expect("mints");

        assert!(tickets.spend(&ticket, StreamSurface::Composition, now()));
        assert!(!tickets.spend(&ticket, StreamSurface::Composition, now()));
    }

    #[test]
    fn a_ticket_tried_against_the_wrong_surface_is_spent_too() {
        // Otherwise it could be offered to each surface in turn until one
        // took it, which is a ticket for all of them.
        let mut tickets = StreamTickets::new();
        let output_id = ovs_types::id::OutputId::new();
        let (ticket, _) = tickets
            .mint(StreamSurface::Composition, now())
            .expect("mints");

        assert!(!tickets.spend(&ticket, StreamSurface::Output { output_id }, now()));
        assert!(!tickets.spend(&ticket, StreamSurface::Composition, now()));
    }

    #[test]
    fn a_ticket_stops_working_when_it_runs_out() {
        let mut tickets = StreamTickets::new();
        let (ticket, _) = tickets
            .mint(StreamSurface::Composition, now())
            .expect("mints");

        assert!(!tickets.spend(&ticket, StreamSurface::Composition, later(60_001)));
    }

    #[test]
    fn a_ticket_works_right_up_to_the_moment_it_expires() {
        let mut tickets = StreamTickets::new();
        let (ticket, _) = tickets
            .mint(StreamSurface::Composition, now())
            .expect("mints");

        assert!(tickets.spend(&ticket, StreamSurface::Composition, later(59_999)));
    }

    #[test]
    fn something_that_was_never_minted_is_refused() {
        let mut tickets = StreamTickets::new();
        assert!(!tickets.spend("not a ticket", StreamSurface::Composition, now()));
    }

    #[test]
    fn minting_clears_out_what_has_expired() {
        // Nothing sweeps this store, so a ticket nobody spent has to go on its
        // own or a client that mints and never watches leaks one a socket.
        let mut tickets = StreamTickets::new();
        for _ in 0..5 {
            tickets
                .mint(StreamSurface::Composition, now())
                .expect("mints");
        }
        assert_eq!(tickets.len(), 5);

        tickets
            .mint(StreamSurface::Composition, later(60_001))
            .expect("mints");
        assert_eq!(tickets.len(), 1, "only the new one is left");
    }

    #[test]
    fn a_client_that_mints_and_never_watches_cannot_grow_the_store_without_end() {
        let mut tickets = StreamTickets::new();
        for _ in 0..OUTSTANDING {
            tickets
                .mint(StreamSurface::Composition, now())
                .expect("mints");
        }

        assert!(
            tickets.mint(StreamSurface::Composition, now()).is_err(),
            "the store is full and says so"
        );
        assert_eq!(tickets.len(), OUTSTANDING);
    }
}
