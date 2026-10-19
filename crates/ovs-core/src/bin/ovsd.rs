//! `ovsd` — the OpenVideoServer core daemon.
//!
//! Owns the authoritative show and live state and serves the client API. It
//! does not render: the renderer is a separate process, so that `ovsd` stopping
//! or being restarted cannot interrupt show output.
//!
//! Configuration comes from the environment, because that is what a systemd unit
//! can set without a config file having to exist before first boot:
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `OVS_BIND` | `0.0.0.0:7420` | address to listen on |
//! | `OVS_CONFIG` | `/etc/openvideoserver/config.json` | the machine's own configuration |
//! | `OVS_DATA_DIR` | `/var/lib/ovs` | where the database and media live |
//! | `OVS_RENDERER` | `127.0.0.1:7421` | where the renderer is listening |
//! | `OVS_DMX` | `on` | `off` to open no Art-Net or sACN sockets |
//! | `OVS_DMX_INTERFACE` | `0.0.0.0` | which interface joins sACN multicast groups |
//! | `OVS_TRUST_LOOPBACK` | `1` | `0` to make a request from this machine sign in like any other |
//! | `OVS_LOG` | `info` | log filter |
//!

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ovs_core::server::startup::ensure_a_show_exists;
use ovs_core::{dmx_link, renderer_link, router, AppState};
use ovs_db::Database;
use ovs_dmx::receiver::ReceiverConfig;
use ovs_types::config::StartupPolicy;

const DEFAULT_BIND: &str = "0.0.0.0:7420";

/// Where the browser pages are served, as well as on the API's own port.
///
/// **Eighty, because that is what somebody types.** A person who knows a
/// machine's address and wants to see what it is doing puts the address in a
/// browser and adds nothing to it, and a server that answered only on 7420
/// would be a machine that looks switched off to everybody but its client.
///
/// One router on two listeners rather than a second application: the page and
/// the API it calls are then the same origin, which is what lets a session
/// cookie work at all without a proxy in between.
const DEFAULT_WEB_BIND: &str = "0.0.0.0:80";

/// The environment variable that moves the browser port, or switches it off.
///
/// Empty turns it off outright, for a machine where something else owns port
/// eighty. The API's own port is unaffected: the client never needed this one.
const WEB_BIND_ENV: &str = "OVS_WEB_BIND";

/// Creates an account and exits.
///
/// For a machine being commissioned over a serial cable or an ssh session, and
/// for the installer's own last step. The browser at `/admin` does the same
/// thing and is the ordinary way; this is the one that works on a server with
/// no screen and no client to hand.
const ADD_ACCOUNT_FLAG: &str = "--add-account";

/// Where the kernel says what is plugged into what.
const DRM_SYSFS: &str = "/sys/class/drm";

/// How often what a restart should restore is written to the database.
///
/// Long enough that a desk riding a fader at 44 Hz costs one small write every
/// five seconds rather than one per frame; short enough that a power cut costs
/// an operator five seconds of the show rather than an evening's worth. Nothing
/// is written when nothing has changed, so on a still show this costs a
/// comparison.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);

/// How often the machine's own state is sampled.
///
/// A second is what a console watching a show wants; anything faster measures
/// the sampler as much as the machine.
const HOST_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_DATA_DIR: &str = "/var/lib/ovs";
const DEFAULT_RENDERER: &str = "127.0.0.1:7421";

#[tokio::main]
async fn main() -> ExitCode {
    let filter = tracing_subscriber::EnvFilter::try_from_env("OVS_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    // One argument, and it does not start a server: it writes the file the
    // System screen's compositor is about to read, and stops. Done here rather
    // than in the daemon because the two used to start in the same second, and
    // a compositor that reads the file one second before it is written takes
    // the wrong panel for as long as the machine is up (§20).
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    // The one argument that does not start a server. It touches the database
    // and nothing else, so it works on a machine where the service is stopped —
    // which is what an installer has just finished doing.
    if let Some(position) = arguments.iter().position(|a| a == ADD_ACCOUNT_FLAG) {
        let (Some(username), Some(password)) =
            (arguments.get(position + 1), arguments.get(position + 2))
        else {
            tracing::error!("{ADD_ACCOUNT_FLAG} wants a name and a password");
            return ExitCode::FAILURE;
        };
        return add_account(username, password);
    }

    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Says which connector the System screen is on, and whether it is plugged in.
///
/// Out loud, once, because it is the one setting that has to be right before
/// anything else can be — and a server that read it silently would give an
/// operator no way to tell a typo from a missing file.
fn report_service_display(configuration: &ovs_core::ServerConfig) {
    // Named `screen` rather than `display`, which is a field shorthand inside
    // the macro and would stop meaning the panel.
    let Some(screen) = &configuration.service_display else {
        tracing::info!(
            path = %ovs_core::ServerConfig::path().display(),
            "no System screen is configured, so every connector is a show's to use"
        );
        return;
    };

    let panel = screen.panel_status(std::path::Path::new(DRM_SYSFS));
    tracing::info!(
        gpu = %screen.gpu_pci_address,
        connector = %screen.connector,
        panel = panel.as_deref().unwrap_or("not checked"),
        "the System screen's connector is reserved; the renderer will not drive it"
    );

    if panel.as_deref() == Some("disconnected") {
        // The difference between a black screen somebody has to drive to the
        // venue to look at, and a line in a log.
        tracing::warn!(
            connector = screen.drm_connector.as_deref().unwrap_or_default(),
            "the System screen's panel is not plugged in"
        );
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let data_dir: PathBuf = std::env::var("OVS_DATA_DIR")
        .unwrap_or_else(|_| DEFAULT_DATA_DIR.to_owned())
        .into();
    std::fs::create_dir_all(&data_dir)?;

    let database_path = data_dir.join("ovs.db");
    tracing::info!(path = %database_path.display(), "opening database");
    let database = Database::open(&database_path)?;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        commit = ovs_types::BUILD_COMMIT,
        "ovsd"
    );
    tracing::info!(version = database.schema_version()?, "database ready");

    // Media lives beside the database, under the same data directory, so a
    // backup of one is a backup of the other and a server is moved by moving one
    // directory.
    let media_root = data_dir.join("media");
    let state = AppState::with_media_root(database, env!("CARGO_PKG_VERSION"), &media_root);
    if let Some(store) = state.media_store() {
        if let Err(error) = store.ensure_root() {
            // Not fatal. A server that cannot write media can still serve
            // clients and still render what it already has, and refusing to
            // start would leave nothing to fix it from.
            tracing::error!(
                path = %media_root.display(),
                "could not create the media directory: {error}"
            );
        }
    }
    tracing::info!(path = %media_root.display(), "media store ready");
    // Before anything is served, which is the only moment at which a file with no
    // row is certainly abandoned rather than an upload halfway through being
    // committed. See `MediaStore::sweep`.
    sweep_the_store(&state);
    // What an operator last decided about previews, before anything renders
    // for them.
    state.load_previews_enabled();
    restore_startup_show(&state)?;

    // The renderer link runs on its own thread. The renderer may not be up yet,
    // or may never be; neither stops the core from serving clients.
    let renderer_address =
        std::env::var("OVS_RENDERER").unwrap_or_else(|_| DEFAULT_RENDERER.to_owned());
    let link_running = Arc::new(AtomicBool::new(true));
    let link_state = state.clone();
    let link_address = renderer_address.clone();
    let link_flag = Arc::clone(&link_running);
    let link_thread = std::thread::Builder::new()
        .name("ovs-renderer-link".to_owned())
        .spawn(move || renderer_link::run(&link_state, &link_address, &link_flag))?;

    // DMX receive, on its own thread for the same reason as the renderer
    // link: blocking sockets and a fixed tick, with nothing between a fader and
    // the show. A machine with no lighting network still opens the sockets, and
    // a machine that cannot open them still serves clients.
    let dmx_running = Arc::new(AtomicBool::new(true));
    let dmx_thread = match dmx_config() {
        Some(config) => {
            let dmx_state = state.clone();
            let dmx_flag = Arc::clone(&dmx_running);
            Some(
                std::thread::Builder::new()
                    .name("ovs-dmx-link".to_owned())
                    .spawn(move || dmx_link::run(&dmx_state, &config, &dmx_flag))?,
            )
        }
        None => {
            tracing::info!("DMX receive disabled by OVS_DMX");
            None
        }
    };

    // Saving what a restart should restore. On its own task rather than on
    // every state change, because a console drives the master at forty-four
    // frames a second and none of that belongs in a database.
    let snapshot_state = state.clone();
    let snapshot_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(SNAPSHOT_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if snapshot_state.persist_live_snapshot() {
                tracing::debug!("live snapshot saved");
            }
        }
    });

    // There was a third task here, waking five hundred times a second to see
    // whether a quantized trigger's boundary had arrived. The owner removed the
    // trigger grid on 2026-09-23 — "Trigger Grid weg." — so a pad takes effect
    // in the request that pressed it and nothing is left to poll for.

    // What the machine is doing, for the Service Console.
    // Sampled here because CPU utilisation is a difference between two readings
    // and a request cannot take both.
    let host_state = state.clone();
    let host_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HOST_SAMPLE_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let sampling = host_state.clone();
            // On a blocking thread: reading /proc, or the Windows equivalent,
            // is a syscall storm and has no business on an async worker.
            if tokio::task::spawn_blocking(move || sampling.sample_host())
                .await
                .is_err()
            {
                return;
            }
        }
    });

    let bind: SocketAddr = std::env::var("OVS_BIND")
        .unwrap_or_else(|_| DEFAULT_BIND.to_owned())
        .parse()?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, renderer = %renderer_address, "ovsd listening");

    // The machine's own configuration, once, out loud. The kiosk's own unit
    // writes its environment from the same file, so this only reports.
    let config = ovs_core::ServerConfig::load();
    report_service_display(&config);

    // **Where this server is, so nobody has to type it** (owner, 2026-09-24).
    // Kept alive for as long as the server is: dropping it withdraws the
    // announcement, so a machine that has been switched off stops being offered
    // instead of being clicked and found missing.
    //
    // A failure here is logged and nothing else. A server that refused to start
    // because a discovery beacon could not open a socket would have turned a
    // convenience into a single point of failure — and the address an operator
    // typed before still works.
    let name = config
        .name
        .clone()
        .unwrap_or_else(ovs_core::discovery::default_name);
    let needs_setup = state
        .with_database(|database| database.account_count())
        .unwrap_or(0)
        == 0;
    let _announcement = match ovs_core::discovery::advertise(&ovs_core::discovery::Announcement {
        name: &name,
        port: bind.port(),
        version: env!("CARGO_PKG_VERSION"),
        api_version: ovs_types::version::API_VERSION,
        needs_setup,
    }) {
        Ok(announcement) => Some(announcement),
        Err(error) => {
            tracing::warn!(
                "this server will not announce itself on the network: {error}; \
                 clients can still reach it at the address above"
            );
            None
        }
    };

    if ovs_core::server::trusts_loopback() {
        // Said out loud, once, because it is a decision about who may stop the
        // show: anything on this machine may, without a password. The System
        // screen is why.
        tracing::info!(
            "requests from this machine are trusted and need no session; \
             set {}=0 to require one",
            ovs_core::server::TRUST_ENV
        );
    }

    // The browser's port, beside the client's. Binding below 1024 needs a
    // capability the unit grants (`AmbientCapabilities=CAP_NET_BIND_SERVICE`),
    // and a failure here is reported and survived: a server that would not
    // start because a status page could not be served would be a worse fault
    // than the page being missing.
    let web = web_bind();
    let web_task = match web {
        Some(address) => match tokio::net::TcpListener::bind(address).await {
            Ok(web_listener) => {
                tracing::info!(%address, "serving the status and administration pages");
                let web_router = router(state.clone());
                Some(tokio::spawn(async move {
                    let _ = axum::serve(
                        web_listener,
                        web_router.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .await;
                }))
            }
            Err(error) => {
                tracing::warn!(
                    %address,
                    "the status and administration pages will not be served: {error}; \
                     they are still on the API's own port"
                );
                None
            }
        },
        None => None,
    };

    axum::serve(
        listener,
        router(state.clone()).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    if let Some(task) = web_task {
        task.abort();
    }

    link_running.store(false, Ordering::Relaxed);
    let _ = link_thread.join();
    dmx_running.store(false, Ordering::Relaxed);
    if let Some(dmx_thread) = dmx_thread {
        let _ = dmx_thread.join();
    }

    // The last thing before the process ends: what an operator had on stage is
    // what they should find when the server comes back.
    snapshot_task.abort();
    host_task.abort();
    if state.persist_live_snapshot() {
        tracing::info!("live snapshot saved");
    }

    tracing::info!("ovsd stopped");
    Ok(())
}

/// Returns what to listen for, or `None` when DMX is switched off.
///
/// On by default. A media server is a lighting fixture as far as a console is
/// concerned, and one that had to be told to listen would be one that silently
/// does nothing on the first night somebody patches it.
fn dmx_config() -> Option<ReceiverConfig> {
    if std::env::var("OVS_DMX").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    }) {
        return None;
    }

    // Which interface the sACN multicast joins go out of. Picking the wrong one
    // on a machine with a separate lighting network is the usual reason a
    // receiver sees nothing at all, so it is worth being able to say.
    let interface = match std::env::var("OVS_DMX_INTERFACE") {
        Ok(value) => match value.trim().parse::<Ipv4Addr>() {
            Ok(address) => address,
            Err(error) => {
                tracing::warn!(
                    value,
                    "OVS_DMX_INTERFACE is not an IPv4 address ({error}); using every interface"
                );
                Ipv4Addr::UNSPECIFIED
            }
        },
        Err(_) => Ipv4Addr::UNSPECIFIED,
    };

    Some(ReceiverConfig {
        interface,
        ..ReceiverConfig::default()
    })
}

/// Opens whatever the startup policy says this server opens (owner decision Q6).
///
/// A failure here is logged and the server still starts. Coming up with black
/// outputs and a readable error beats refusing to start, because a server that
/// will not start cannot be fixed from the client.
/// Deletes storage objects the library no longer references.
///
/// Until 2026-09-25 deleting media deleted the row and left the file, so every
/// store that has had anything deleted from it is carrying objects nothing can
/// reach: not in the library, named by no show, resolved by no request. The only
/// thing they do is take up the disc a show needs.
///
/// Nothing here is fatal. A sweep that cannot read the library leaves the disc
/// exactly as it is, and a server refusing to start over housekeeping would be
/// worse than one carrying some dead files for another day.
fn sweep_the_store(state: &AppState) {
    let Some(store) = state.media_store() else {
        return;
    };
    let referenced = match state.with_database(|database| database.referenced_object_keys()) {
        Ok(referenced) => referenced,
        Err(error) => {
            tracing::warn!("nothing was swept, because the library could not be read: {error}");
            return;
        }
    };

    let swept = store.sweep(&referenced);
    if swept.objects > 0 {
        tracing::info!(
            objects = swept.objects,
            freed_bytes = swept.bytes,
            referenced = referenced.len(),
            "swept media nothing references any more"
        );
    }
}

fn restore_startup_show(state: &AppState) -> Result<(), Box<dyn std::error::Error>> {
    // The DMX switch first, as it was left (owner, 2026-10-19). It is the
    // server's rather than a show's, so it is put back whether or not there is
    // a show to open, and opening one leaves it as it is.
    match state.with_database(|database| database.dmx_enabled()) {
        Ok(enabled) => {
            state.mutate_state(|current| {
                let change = current.update_master(|master| master.dmx_enabled = enabled);
                ((), change)
            });
            if !enabled {
                tracing::info!(
                    "DMX was switched off in Live when this server stopped, and still is"
                );
            }
        }
        Err(error) => tracing::warn!("the DMX switch could not be read, so it is on: {error}"),
    }

    // Before the policy is read, because it decides *which* show to open and
    // this decides whether there is one to open at all. A failure here is not
    // a reason to refuse to start: a server with no show is what this build
    // did until now, and it is still a server.
    match ensure_a_show_exists(state) {
        Ok(Some(id)) => tracing::info!(show = %id, "this server had no show, so it made one"),
        Ok(None) => {}
        Err(error) => tracing::error!("could not make this server a first show: {error}"),
    }

    let policy = state.with_database(|database| database.startup_policy())?;

    let show_id = match policy {
        StartupPolicy::None => {
            // The operator asked for black at startup, and a show made a
            // moment ago does not overrule that. It is in the list, ready to
            // be loaded by hand.
            tracing::info!("startup policy: no show");
            return Ok(());
        }
        StartupPolicy::Pinned { show_id } => Some(show_id),
        StartupPolicy::LastActive => state
            .with_database(|database| database.list_shows())?
            .first()
            .map(|summary| summary.id),
    };

    let Some(show_id) = show_id else {
        tracing::info!("no show to restore");
        return Ok(());
    };

    let loaded = state.with_database(|database| {
        let show = database.load_show(show_id)?;
        let snapshot = database.load_live_snapshot(show_id)?;
        Ok::<_, ovs_db::DbError>((show, snapshot))
    });

    match loaded {
        Ok((show, snapshot)) => {
            let name = show.name.clone();
            state.mutate_state(|current| {
                let change = current.load_show(show, snapshot.as_ref());
                ((), Some(change))
            });
            tracing::info!(show = %name, "restored show");
        }
        Err(error) => {
            tracing::error!(%show_id, "could not restore show: {error}");
        }
    }

    Ok(())
}

/// Where the browser pages are served, or nothing when switched off.
fn web_bind() -> Option<SocketAddr> {
    let value = std::env::var(WEB_BIND_ENV).unwrap_or_else(|_| DEFAULT_WEB_BIND.to_owned());
    if value.trim().is_empty() {
        tracing::info!("{WEB_BIND_ENV} is empty, so no browser port is opened");
        return None;
    }
    match value.parse() {
        Ok(address) => Some(address),
        Err(error) => {
            tracing::warn!("{WEB_BIND_ENV}={value} is not an address: {error}");
            None
        }
    }
}

/// Creates an account from the command line.
///
/// **The first one administers; later ones drive the show.** Same rule as the
/// setup endpoint, and for the same reason: the first account is the only thing
/// that can make the second, so a server whose only account cannot administer it
/// is a server nobody can finish setting up.
///
/// The password is taken as an argument, which means it is visible in the
/// process list for as long as this runs and stays in the shell's history. That
/// is stated in the installer, which reads it into a variable rather than
/// typing it on a line, and it is why the browser at `/admin` is the ordinary
/// way in.
fn add_account(username: &str, password: &str) -> ExitCode {
    let directory = std::env::var("OVS_DATA_DIR").unwrap_or_else(|_| DEFAULT_DATA_DIR.to_owned());
    let path = std::path::Path::new(&directory).join("ovs.db");

    let mut database = match ovs_db::Database::open(&path) {
        Ok(database) => database,
        Err(error) => {
            tracing::error!(path = %path.display(), "cannot open the database: {error}");
            return ExitCode::FAILURE;
        }
    };

    let username = username.trim();
    if username.is_empty() {
        tracing::error!("a username is required");
        return ExitCode::FAILURE;
    }
    if password.is_empty() {
        tracing::error!("a password is required");
        return ExitCode::FAILURE;
    }

    match database.account_by_username(username) {
        Ok(Some(_)) => {
            tracing::error!("an account called {username} already exists");
            return ExitCode::FAILURE;
        }
        Ok(None) => {}
        Err(error) => {
            tracing::error!("cannot read the accounts: {error}");
            return ExitCode::FAILURE;
        }
    }

    let first = matches!(database.account_count(), Ok(0));
    let role = if first {
        ovs_types::account::Role::Admin
    } else {
        ovs_types::account::Role::Operator
    };

    let password_hash = match ovs_core::auth::hash_password(password) {
        Ok(hash) => hash,
        Err(error) => {
            tracing::error!("cannot hash the password: {error}");
            return ExitCode::FAILURE;
        }
    };

    match database.upsert_account(&ovs_db::Account {
        id: ovs_types::id::AccountId::new(),
        username: username.to_owned(),
        password_hash,
        role,
        created_at: ovs_types::time::Timestamp::now(),
        last_login_at: None,
    }) {
        Ok(()) => {
            tracing::info!("created {username} as {role}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!("cannot store the account: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => tracing::info!("interrupt received"),
        () = terminate => tracing::info!("terminate received"),
    }
}
