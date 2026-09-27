//! The machine's own settings, as opposed to the show's.
//!
//! The owner, 2026-09-24: "auch systemsettings wie das interne System Display
//! ändern kann, sprich den Globalen Output zuweisen kann."
//!
//! Two things live in `/etc/openvideoserver/config.json` and they are the two
//! facts about a machine that have to be true before anything else can be: what
//! it calls itself, and which connector carries the System screen.
//! [`crate::config_file`] says why they are in a file rather than the database.
//!
//! # This takes effect at the next start, and says so
//!
//! Nothing here changes a running machine. The connector is reserved when the
//! renderer starts, and the kiosk is pointed at a card before the compositor
//! opens it — both of them read this file once, early, precisely so that
//! neither can take the panel first. Writing it mid-show is therefore safe and
//! does nothing visible, which is the honest thing for the page to say rather
//! than implying a Save button moved a screen.
//!
//! # Two places remember the System screen
//!
//! This file, and the connector roles in the database that a client edits. The
//! file wins, because it is the only one of the two that can be read before the
//! renderer has started. They can disagree — somebody may have assigned
//! `SERVICE_DISPLAY` to a different socket in the client — and when they do,
//! this reports it rather than quietly making them agree. A server that
//! silently overwrote a decision somebody made in the client would be a server
//! that lost an argument nobody knew was happening.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use ovs_proto::http::{
    ServiceDisplayChoice, ServiceDisplayInfo, SetSystemConfigRequest, SystemConfigResponse,
};

use crate::config_file::{ConnectorRef, ServerConfig};
use crate::server::app_state::AppState;
use crate::server::auth_routes::administer;
use crate::server::error::{ApiFailure, ApiResult};

/// What this machine is, right now.
pub async fn read_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<SystemConfigResponse>> {
    administer(&state, &headers)?;
    Ok(Json(describe(&ServerConfig::load(), &state)))
}

/// Changes it.
/// The name of the show output on this socket, if there is one.
///
/// **A socket does one job** (owner, 2026-09-25: "Media oder System blockt den
/// Output für jeweils beides. Nur Unused/Free kann für alles benutzt werden").
/// The other direction was already kept: a socket carrying the System screen is
/// refused to the show, because the configuration file wins over every derived
/// role and the client refuses to put an output on one.
///
/// Read off the show rather than off the merged roles. The merged roles contain
/// this very connector as `SERVICE_DISPLAY` when it is the one already set, so
/// asking them would refuse a request to leave the setting exactly where it is.
///
/// Disabled outputs count. One switched off is one an operator is coming back to,
/// and it holds its socket for exactly that reason.
fn output_on(show: &ovs_types::config::Show, wanted: &ConnectorRef) -> Option<String> {
    show.outputs
        .iter()
        .find(|output| {
            output.identity.gpu_pci_address.as_deref() == Some(wanted.gpu_pci_address.as_str())
                && output
                    .identity
                    .connector
                    .as_deref()
                    .is_some_and(|connector| connector == wanted.connector)
        })
        .map(|output| output.name.clone())
}

pub async fn write_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SetSystemConfigRequest>,
) -> ApiResult<Json<SystemConfigResponse>> {
    administer(&state, &headers)?;

    let current = ServerConfig::load();
    let mut next = ServerConfig {
        name: clean_name(request.name)?,
        service_display: None,
    };

    if let Some(choice) = request.service_display {
        let wanted = chosen(&choice, current.service_display.as_ref())?;
        let taken =
            state.with_state(|current| current.show().and_then(|show| output_on(show, &wanted)));
        if let Some(taken) = taken {
            return Err(ApiFailure::conflict(
                "connector_in_use",
                format!(
                    "{} on {} is carrying the output \"{taken}\". A socket does one job:                      remove that output from the show, and this socket is free again.",
                    wanted.connector, wanted.gpu_pci_address
                ),
            ));
        }
        next.service_display = Some(wanted);
    }

    let path = ServerConfig::path();
    next.write(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::PermissionDenied
            || error.kind() == std::io::ErrorKind::ReadOnlyFilesystem
        {
            // The one failure worth explaining rather than reporting, because
            // the cause is three layers away from anybody reading it and the
            // fix is one line in a unit file.
            ApiFailure::internal(format!(
                "cannot write {}: the service is not allowed to. Its systemd unit needs \
                 ReadWritePaths={} , or edit the file by hand on the machine.",
                path.display(),
                path.parent().unwrap_or(&path).display(),
            ))
        } else {
            ApiFailure::internal(format!("cannot write {}: {error}", path.display()))
        }
    })?;

    // **Now, not at the next start.** This used to be true the other way: the
    // System screen was a kiosk compositor that took a card, and moving it meant
    // restarting the unit that held one. It is drawn off-screen and shown by the
    // renderer now, and the renderer re-reads the connector roles once a second —
    // so a socket reassigned at half past eight moves the screen at half past
    // eight. Telling it is this flag.
    state.connector_roles_changed();

    tracing::info!(
        path = %path.display(),
        name = ?next.name,
        service_display = ?next.service_display.as_ref().map(|display| {
            format!("{}/{}", display.gpu_pci_address, display.connector)
        }),
        mode = ?next.service_display.as_ref().and_then(|display| display.mode),
        "the machine's configuration was changed"
    );

    Ok(Json(describe(&next, &state)))
}

/// A name somebody typed, or nothing.
///
/// Whitespace on its own is nothing: a field cleared by selecting and deleting
/// often leaves a space behind, and a server called " " is a server nobody can
/// pick out of a list.
fn clean_name(name: Option<String>) -> Result<Option<String>, ApiFailure> {
    let Some(name) = name else { return Ok(None) };
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > 64 {
        return Err(ApiFailure::bad_request(
            "name_too_long",
            "a server name is at most 64 characters",
        ));
    }
    Ok(Some(name.to_owned()))
}

/// The chosen connector, carrying over what still applies to it.
///
/// `drm_device` and `drm_connector` are the kernel's names for the *same*
/// socket, and they are how the server says at startup whether the panel is
/// plugged in. They are kept when the socket is unchanged and **dropped when it
/// is not**: a kernel name left over from the previous connector describes
/// another socket entirely, and the answer it produces would be confident and
/// wrong. Losing a hand-written override is recoverable; being told the wrong
/// panel is fine is what sends somebody on a drive.
fn chosen(
    choice: &ServiceDisplayChoice,
    current: Option<&ConnectorRef>,
) -> Result<ConnectorRef, ApiFailure> {
    let gpu = choice.gpu_pci_address.trim();
    let connector = choice.connector.trim();
    if gpu.is_empty() || connector.is_empty() {
        return Err(ApiFailure::bad_request(
            "incomplete_connector",
            "a connector needs both a GPU address and a connector name",
        ));
    }

    let same = current.is_some_and(|had| had.gpu_pci_address == gpu && had.connector == connector);

    Ok(ConnectorRef {
        gpu_pci_address: gpu.to_owned(),
        connector: connector.to_owned(),
        drm_connector: if same {
            current.and_then(|had| had.drm_connector.clone())
        } else {
            None
        },
        // Whatever was sent, including nothing — "nothing" is the panel's own
        // preference, which is a choice an operator can make deliberately after
        // trying a mode that did not work.
        //
        // Not carried over from `current` when the connector is the same,
        // unlike the two above: those are names for the socket this *is*, and a
        // mode is a decision about it. A client that sends a connector without
        // a mode is saying "this socket, panel's choice".
        mode: rate_of(choice.mode.as_ref())?,
    })
}

/// Turns a mode a client picked off the list into one the renderer can be told.
///
/// The conversion is here because `FrameRate::from_millihertz` is the one place
/// that knows 59940 is 60000/1001 rather than 59940/1000 — the two are different
/// rates that round to the same three decimals, and a panel set to the wrong one
/// tears. A client is not asked to know it.
fn rate_of(
    mode: Option<&ovs_proto::http::ConnectorModeInfo>,
) -> Result<Option<ovs_types::config::DisplayMode>, ApiFailure> {
    let Some(mode) = mode else {
        return Ok(None);
    };

    let refresh = ovs_types::geometry::FrameRate::from_millihertz(mode.refresh_millihertz)
        .ok_or_else(|| {
            ApiFailure::bad_request(
                "unreadable_mode",
                format!(
                    "{} mHz is not a refresh rate this server can name. Pick a mode from the \
                     list the renderer reported for this connector.",
                    mode.refresh_millihertz
                ),
            )
        })?;

    Ok(Some(ovs_types::config::DisplayMode {
        size: mode.size,
        refresh,
    }))
}

/// Describes a configuration for the page.
fn describe(config: &ServerConfig, state: &AppState) -> SystemConfigResponse {
    let found = state.connectors();
    let path = ServerConfig::path();

    SystemConfigResponse {
        name: config.name.clone(),
        host_name: host_name(),
        service_display: config
            .service_display
            .as_ref()
            .map(|display| ServiceDisplayInfo {
                present: found.iter().any(|candidate| {
                    candidate.gpu_pci_address == display.gpu_pci_address
                        && candidate.connector == display.connector
                }),
                gpu_pci_address: display.gpu_pci_address.clone(),
                connector: display.connector.clone(),
                // Back in the shape the list is in, so a client can compare it
                // against the list without knowing how a rate is written.
                mode: display.mode.and_then(|mode| {
                    mode.refresh.millihertz().map(|refresh_millihertz| {
                        ovs_proto::http::ConnectorModeInfo {
                            size: mode.size,
                            refresh_millihertz,
                        }
                    })
                }),
            }),
        writable: writable(&path),
        path: path.display().to_string(),
    }
}

/// What this machine calls itself when nothing else does.
fn host_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "this machine".to_owned())
}

/// Whether the configuration can actually be written.
///
/// Answered by writing, because every cheaper answer is a guess. The unit runs
/// under `ProtectSystem=strict`, so the mode bits on the directory say nothing
/// about whether this process may write it — the sandbox refuses first and the
/// permissions never come into it. The file it makes is named after the real
/// one and removed immediately.
fn writable(path: &std::path::Path) -> bool {
    let Some(directory) = path.parent() else {
        return false;
    };
    let mut probe = path.as_os_str().to_owned();
    probe.push(".probe");
    let probe = std::path::PathBuf::from(probe);

    if std::fs::create_dir_all(directory).is_err() {
        return false;
    }
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(gpu: &str, connector: &str) -> ServiceDisplayChoice {
        ServiceDisplayChoice {
            gpu_pci_address: gpu.to_owned(),
            connector: connector.to_owned(),
            mode: None,
        }
    }

    fn had(connector: &str) -> ConnectorRef {
        ConnectorRef {
            gpu_pci_address: "0000:00:02.0".to_owned(),
            connector: connector.to_owned(),
            drm_connector: Some("eDP-1".to_owned()),
            mode: None,
        }
    }

    /// A show with one output on `connector`, or on nothing.
    fn show_with(connector: Option<&str>, gpu: &str) -> ovs_types::config::Show {
        let mut show = ovs_types::config::Show::new("Show");
        let mut output = ovs_types::config::OutputConfig::new(
            "LED Wall Left",
            ovs_types::config::DisplayMode::new(
                ovs_types::geometry::SizePx::new(1920, 1080),
                ovs_types::geometry::FrameRate::FPS_60,
            ),
        );
        output.identity = ovs_types::config::OutputIdentity {
            gpu_pci_address: Some(gpu.to_owned()),
            connector: connector.map(str::to_owned),
            ..ovs_types::config::OutputIdentity::default()
        };
        show.outputs.push(output);
        show
    }

    #[test]
    fn a_socket_the_show_is_using_cannot_carry_the_system_screen() {
        // **A socket does one job** (owner, 2026-09-25: "Media oder System blockt
        // den Output für jeweils beides"). The other direction was already kept;
        // this one was checked nowhere, so the page offered both walls and the
        // server wrote whatever it was told.
        //
        // The output's name comes back, because "this socket is taken" leaves
        // somebody hunting for what took it.
        let show = show_with(Some("HDMI-0"), "0000:01:00.0");
        let wanted = ConnectorRef {
            gpu_pci_address: "0000:01:00.0".to_owned(),
            connector: "HDMI-0".to_owned(),
            drm_connector: None,
            mode: None,
        };

        assert_eq!(output_on(&show, &wanted).as_deref(), Some("LED Wall Left"));
    }

    #[test]
    fn a_free_socket_can_carry_it() {
        let show = show_with(Some("HDMI-0"), "0000:01:00.0");
        let free = ConnectorRef {
            gpu_pci_address: "0000:01:00.0".to_owned(),
            connector: "DP-4".to_owned(),
            drm_connector: None,
            mode: None,
        };

        assert_eq!(output_on(&show, &free), None);
    }

    #[test]
    fn the_same_connector_name_on_another_card_is_another_socket() {
        // `DP-1` exists on every card in the machine, which is why the address
        // is half of the identity.
        let show = show_with(Some("HDMI-0"), "0000:01:00.0");
        let elsewhere = ConnectorRef {
            gpu_pci_address: "0000:00:02.0".to_owned(),
            connector: "HDMI-0".to_owned(),
            drm_connector: None,
            mode: None,
        };

        assert_eq!(output_on(&show, &elsewhere), None);
    }

    #[test]
    fn a_stream_holds_no_socket() {
        // An output published on the network names no connector, and must not
        // block one.
        let show = show_with(None, "0000:01:00.0");
        let wanted = ConnectorRef {
            gpu_pci_address: "0000:01:00.0".to_owned(),
            connector: "HDMI-0".to_owned(),
            drm_connector: None,
            mode: None,
        };

        assert_eq!(output_on(&show, &wanted), None);
    }

    #[test]
    fn the_kernel_names_survive_a_save_that_changes_nothing() {
        let current = had("monitor");
        let next = chosen(&choice("0000:00:02.0", "monitor"), Some(&current))
            .expect("a fully named connector is accepted");

        assert_eq!(
            next.drm_connector.as_deref(),
            Some("eDP-1"),
            "the kernel's name for the same socket is still the kernel's name for it, and it is \
             the only thing that can say whether the panel is plugged in"
        );
    }

    #[test]
    fn the_kernel_names_go_when_the_socket_does() {
        let current = had("monitor");
        let next = chosen(&choice("0000:00:02.0", "DP-3"), Some(&current))
            .expect("a fully named connector is accepted");

        assert_eq!(
            next.drm_connector, None,
            "eDP-1 was the kernel's name for the panel, not for DP-3; kept, it would report \
             confidently about the wrong socket"
        );
    }

    #[test]
    fn a_different_card_is_a_different_socket() {
        let current = had("monitor");
        let next = chosen(&choice("0000:01:00.0", "monitor"), Some(&current))
            .expect("a fully named connector is accepted");

        assert_eq!(
            next.drm_connector, None,
            "DP-1 exists on every card in the machine, so the same connector name on another \
             card is a different socket"
        );
    }

    #[test]
    fn a_half_named_connector_is_refused_rather_than_guessed() {
        assert!(chosen(&choice("", "DP-1"), None).is_err());
        assert!(chosen(&choice("0000:00:02.0", "   "), None).is_err());
    }

    #[test]
    fn a_name_of_spaces_is_no_name() {
        assert_eq!(
            clean_name(Some("   ".to_owned())).expect("spaces are a valid thing to send"),
            None
        );
        assert_eq!(
            clean_name(Some("  Bühne links  ".to_owned())).expect("a short name is accepted"),
            Some("Bühne links".to_owned())
        );
    }

    #[test]
    fn a_name_nobody_could_read_is_refused() {
        assert!(clean_name(Some("x".repeat(65))).is_err());
        assert!(clean_name(Some("x".repeat(64))).is_ok());
    }
}

#[cfg(test)]
mod mode_tests {
    use super::*;

    fn entry(
        width: u32,
        height: u32,
        refresh_millihertz: u32,
    ) -> ovs_proto::http::ConnectorModeInfo {
        ovs_proto::http::ConnectorModeInfo {
            size: ovs_types::geometry::SizePx { width, height },
            refresh_millihertz,
        }
    }

    #[test]
    fn a_mode_picked_off_the_list_comes_back_as_the_same_entry() {
        // Round-tripped rather than checked one way: the chooser compares what
        // the server reports against the list it was given, and a conversion
        // that is exact one way and approximate the other puts the tick on the
        // wrong line — or on none, which reads as "the panel decides" when
        // something else was chosen.
        for millihertz in [60_000, 59_940, 50_000, 29_970, 23_976, 59_951, 144_000] {
            let picked = entry(1920, 1080, millihertz);
            let stored = rate_of(Some(&picked))
                .expect("a rate")
                .expect("a mode was picked");
            assert_eq!(stored.size, picked.size);
            assert_eq!(
                stored.refresh.millihertz(),
                Some(millihertz),
                "{millihertz} mHz did not survive"
            );
        }
    }

    #[test]
    fn the_broadcast_rates_are_the_exact_ones_and_not_the_rounded_ones() {
        // 59.94 is 60000/1001. 59940/1000 is a different rate that rounds to the
        // same three decimals, and a panel set to it tears. This is the whole
        // reason the conversion is on the server rather than in the page.
        let stored = rate_of(Some(&entry(1920, 1080, 59_940)))
            .expect("a rate")
            .expect("a mode");
        assert_eq!(stored.refresh.numerator, 60_000);
        assert_eq!(stored.refresh.denominator, 1001);

        // And a whole rate stays whole rather than becoming 60000/1000.
        let whole = rate_of(Some(&entry(1024, 600, 60_000)))
            .expect("a rate")
            .expect("a mode");
        assert_eq!(whole.refresh.numerator, 60);
        assert_eq!(whole.refresh.denominator, 1);
    }

    #[test]
    fn no_mode_is_the_panel_deciding_and_not_an_error() {
        assert_eq!(rate_of(None).expect("no failure"), None);
    }

    #[test]
    fn a_rate_this_server_cannot_name_is_refused_rather_than_guessed_at() {
        // Zero is what a driver reports for a mode with no rate. Taking it would
        // write a mode into the machine's configuration that nothing can set,
        // and the panel would go dark at the next start with the file insisting
        // it is fine.
        assert!(rate_of(Some(&entry(1920, 1080, 0))).is_err());
    }
}
