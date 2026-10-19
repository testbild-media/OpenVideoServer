//! The pages a browser gets, on the port a browser tries first.
//!
//! The owner, 2026-09-24: "Port 80 auf dem Server zeigt einen Status an und das
//! man den Clienten braucht zum Steuern, ggf. kann man dort lokal den Clienten
//! für Windows schon runterladen. Da soll es irgendwo einen kleinen Bereich
//! Admin geben, wo man User anlegen kann, Passwort ändern kann, auch
//! systemsettings … ändern kann."
//!
//! # Two pages, two audiences
//!
//! **`/` is for somebody who typed the address into a browser**, because that is
//! what one does with a machine on a network. It says what this is, whether it
//! is running, and that driving the show needs the client — and it offers the
//! client. It names the show that is open and says how the machine is
//! coping; it says nothing about what is *in* the show. Anybody on the
//! segment can read it.
//!
//! **`/admin` is the alternative to the Service Display.** A rack unit with no
//! screen is a legitimate server — `ServerConfig::service_display` is optional
//! on purpose — and such a machine could until now only be administered from a
//! client. This is where accounts and the machine's own settings live for one.
//!
//! # What this is served over
//!
//! **Plain HTTP, by the owner's decision of 2026-09-24**, asked directly and
//! answered "nur HTTP, dem LAN wird vertraut". A password typed into the
//! sign-in form therefore crosses the network in clear, and anything that can
//! watch the segment can read it. That is a defensible choice for a closed show
//! network and a bad one for a venue's guest wifi, and **the sign-in page says
//! so in as many words** — the person who needs to know is the one about to
//! type.
//!
//! Everything that does not depend on trusting the network is still done: the
//! session cookie is `HttpOnly` and `SameSite=Strict`, passwords are stored as
//! Argon2 hashes, and no page ever renders one.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::server::app_state::AppState;

/// The status page, for anybody who reaches this machine.
pub async fn status_page() -> Response {
    page(include_str!("../../../../web/status.html"))
}

/// The administration page.
///
/// Served to anybody, because it is the API underneath that refuses. A page
/// that answered 404 to a signed-out browser would be a page nobody could ever
/// sign in to.
pub async fn admin_page() -> Response {
    page(include_str!("../../../../web/admin.html"))
}

/// What this machine is doing, for the status page to draw.
///
/// **The only route on this server that answers without a session**, so what is
/// on it is a decision rather than a convenience.
///
/// # What is on it
///
/// The machine and the walls: load, temperatures, memory, disc, how many
/// decoders are running, how many frames arrived late, and for each output
/// whether it is lit, at what size and rate, and how many frames it has dropped.
/// The owner asked for exactly this on 2026-09-24 — "die infos mit frame drops,
/// cpu und gpu usw sollten auf der seite ja stehen ohne login" — and it is the
/// half of the System screen that answers "is this machine healthy", which is
/// the question somebody standing in a rack room has.
///
/// The show's **name**, because the owner asked for it (2026-09-24:
/// "projektname wäre jedoch gut"). It is the first thing somebody wants
/// confirmed when they walk up to a rack — that this machine has tonight's
/// show open and not last week's — and confirming it from a phone beats
/// signing in.
///
/// A missing output is on it, because a wall that should be there and is not is
/// a fault the person in the rack room is looking for.
///
/// # What is not
///
/// The show's **identifier**, which is for machines and tells a person
/// nothing; which clip is on which layer; the tempo; and which DMX universes a
/// desk is driving. Those describe the performance rather than the machine, and
/// nobody has asked to publish them. A name is a label on a box; a running
/// cue list is the show itself.
pub async fn public_status(State(state): State<AppState>) -> Response {
    let (show, renderer) = state.with_state(|current| {
        (
            current.show().map(|show| show.name.clone()),
            current.renderer().clone(),
        )
    });
    let needs_setup = state
        .with_database(|database| database.account_count())
        .map(|count| count == 0)
        .unwrap_or(false);

    let host = state.host();
    let gpu = state.gpu_telemetry();
    let playback = state.playback_telemetry();

    axum::Json(serde_json::json!({
        "name": server_name(),
        "version": state.server_version(),
        "renderer": renderer,
        // Null when nothing is open, which a server between shows genuinely is.
        "show": show,
        "needs_setup": needs_setup,
        "downloads": super::downloads::available(),
        "fixtures": super::desk_fixtures::summary(state.server_version()),
        // Where the installer is, when an update has been asked for. Here as
        // well as behind the administration API because installing restarts
        // the server, sessions live in memory, and the page that asked has to
        // be able to follow it through that restart. It says which version and
        // how far, which is a fact about the machine like its version is.
        "update": crate::update::progress(),
        // A network change on trial, for the same reason: the page that made
        // it may be on an address that has just stopped answering, and the
        // System screen and the new address both have to see that it is
        // waiting to be kept. Which interface, what it was set to and until
        // when — facts about the machine, as its address is.
        "network": crate::network::trial(),
        "uptime_s": state.uptime_s(),
        "machine": {
            "cpu_percent": host.cpu_percent,
            "cpu_temperature_c": host.cpu_temperature_c,
            "ram_percent": host.ram_percent,
            "ram_used_bytes": host.ram_used_bytes,
            "ram_total_bytes": host.ram_total_bytes,
            "disk_used_bytes": host.disk_used_bytes,
            "disk_total_bytes": host.disk_total_bytes,
            // Absent rather than zero on a machine with no card the renderer
            // could open: a card at 0% and a card nobody asked must not look
            // the same on a screen (house rule §16).
            "gpu_percent": gpu.as_ref().and_then(|gpu| gpu.utilisation_percent),
            "gpu_temperature_c": gpu.as_ref().and_then(|gpu| gpu.temperature_c),
            "vram_used_bytes": gpu.as_ref().and_then(|gpu| gpu.vram_used_bytes),
            "vram_total_bytes": gpu.as_ref().and_then(|gpu| gpu.vram_total_bytes),
        },
        "playback": {
            "active_decoders": playback.map_or(0, |playback| playback.playing),
            "decode_errors": playback.map_or(0, |playback| playback.errors),
            "late_frames": playback.map_or(0, |playback| playback.late_frames),
        },
        "frame": state.frame_timings(),
        "outputs": state.output_telemetry(),
        "missing_outputs": state.missing_outputs(),
        "test_pattern": state.test_pattern(),
    }))
    .into_response()
}

/// What this machine calls itself.
///
/// Read per request rather than held: an administrator who renames the server
/// should see the new name on the next reload, and this page is looked at by a
/// person rather than by a loop.
fn server_name() -> String {
    crate::config_file::ServerConfig::load()
        .name
        .unwrap_or_else(crate::discovery::default_name)
}

/// Hands out one of the client installers this machine carries.
pub async fn download(
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Result<Response, StatusCode> {
    let path = super::downloads::resolve(&name).ok_or(StatusCode::NOT_FOUND)?;
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".to_owned(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                // The name is already known to be a plain file name with no
                // separator, quote or newline in it — `resolve` refuses
                // everything else — so it cannot break out of this header.
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Hands out every desk's fixture types, as one ZIP.
///
/// Without a session, like the client: a fixture type says how this server
/// reads a desk, which is what anybody patching one needs and nothing about the
/// show (owner, 2026-10-19: "alle Fixtures als Zip mitliefern").
pub async fn fixtures(State(state): State<AppState>) -> Result<Response, StatusCode> {
    let bytes = super::desk_fixtures::archive().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let name = super::desk_fixtures::file_name(state.server_version());
    Ok((
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/zip".to_owned(),
            ),
            (
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// Serves one compiled-in page.
///
/// Compiled in rather than read from disk, for the reason the Service Console
/// gives: a server is one file to deploy, and a page cannot go missing because
/// somebody tidied a directory.
fn page(html: &'static str) -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}
