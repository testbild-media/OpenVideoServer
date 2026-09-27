//! DMX-over-IP receive, and the mapping from channels to parameters.
//!
//! Two protocols, one universe model, one fixture layer. Which wire a value
//! arrived on is forgotten as early as possible: Art-Net and sACN feed the same
//! universes, and a fixture that behaved differently depending on how a packet
//! reached the machine would be a fixture nobody could reason about.
//!
//! - [`artnet`] — ArtDMX packets.
//! - [`sacn`] — ANSI E1.31 data packets.
//! - [`universe`] — universes, and who is allowed to drive them.
//! - [`mapping`] — the patch, applied: universes in, parameters out.
//! - [`receiver`] — the sockets, and the threads that read them.
//!
//! # Receive only
//!
//! This is a media server, not a lighting node. It does not answer ArtPoll, does
//! not advertise itself and does not transmit: appearing in a lighting network's
//! discovery is a claim about what this machine is, and it is not one to make
//! without an operator asking.

#![forbid(unsafe_code)]
#![warn(missing_docs, clippy::all)]

pub mod artnet;
pub mod mapping;
pub mod receiver;
pub mod sacn;
pub mod universe;
