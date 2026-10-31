use std::{
    future::{Future, pending},
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};

use futures_util::StreamExt as _;
use librespot_connect::{ConnectConfig, Spirc};
use librespot_core::{
    Error, authentication::Credentials, cache::Cache, config::SessionConfig, error::ErrorKind,
    session::Session,
};
use librespot_discovery::{DeviceType, Discovery};
use librespot_playback::{
    config::PlayerConfig,
    mixer::Mixer as _,
    player::{Player, PlayerEvent},
};
use log::{info, warn};
use pulsar_core::{actors, model::Model};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::{Mixer, sink::SpotifySink, volume_to_f32, volume_to_u16};

const CONNECT_RETRY: Duration = Duration::from_secs(15);
const DISCOVERY_RETRY: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(45);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const STABLE_CONNECTION: Duration = Duration::from_secs(10);

/// The playback state shown to the controller. `Idle` covers both "stopped" and "unavailable".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Playback {
    #[default]
    Idle,
    Loading,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    Play,
    Pause,
    Next,
    Previous,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Playback(Playback),
    Volume(f32),
}

enum Cmd {
    Transport(Transport),
    Volume(f32),
}

pub struct Handle {
    cmd: UnboundedSender<Cmd>,
    gain: Arc<AtomicU32>,
}

impl Handle {
    pub fn transport(&self, transport: Transport) {
        let _ = self.cmd.send(Cmd::Transport(transport));
    }

    pub fn volume(&self, volume: f32) {
        let _ = self.cmd.send(Cmd::Volume(volume));
    }

    pub fn gain(&self, gain: f32) {
        self.gain
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

pub struct Config {
    /// Friendly name shown by Spotify.
    pub name: String,
    /// Stable per-station Connect identity (the Pulsar device name).
    pub device_id: String,
    /// `credentials.json` lives here; the directory is created with mode 0700.
    pub state_dir: PathBuf,
    pub model: &'static Model,
}

type Task = Pin<Box<dyn Future<Output = ()>>>;
type Connecting = Pin<Box<dyn Future<Output = Result<(Spirc, Task), Error>>>>;

pub fn spawn(cfg: Config, volume: f32) -> (Handle, UnboundedReceiver<Event>) {
    let (cmd, commands) = mpsc::unbounded_channel();
    let (events, rx) = mpsc::unbounded_channel();
    let gain = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    actors::spawn_actor("spotify", run(cfg, volume, gain.clone(), commands, events));
    (Handle { cmd, gain }, rx)
}

async fn run(
    cfg: Config,
    volume: f32,
    gain: Arc<AtomicU32>,
    mut commands: UnboundedReceiver<Cmd>,
    events: UnboundedSender<Event>,
) {
    let config = SessionConfig {
        device_id: cfg.device_id,
        ..SessionConfig::default()
    };
    let cache = open_cache(&cfg.state_dir);
    let mut credentials = cache.as_ref().and_then(Cache::credentials);
    let (volume_tx, mut volume_rx) = mpsc::unbounded_channel();
    let mixer = Arc::new(Mixer::new(volume_to_u16(volume), volume_tx));
    let mut session = Session::new(config.clone(), cache.clone());
    let player = Player::new(
        PlayerConfig::default(),
        session.clone(),
        mixer.get_soft_volume(),
        move || Box::new(SpotifySink::new(cfg.model, gain)),
    );
    let mut player_events = player.get_player_event_channel();
    let mixer_handle: Arc<dyn librespot_playback::mixer::Mixer> = mixer.clone();

    let mut discovery = start_discovery(&config, &cfg.name);
    let mut spirc: Option<Spirc> = None;
    let mut task: Option<Task> = None;
    let mut connecting: Option<Connecting> = None;
    // A cached credential is retried while it has not produced an unstable session: a connection
    // that closes before STABLE_CONNECTION waits for new credentials from discovery instead of
    // looping forever.
    let mut retry_cached = credentials.is_some();
    let mut reconnect = retry_cached;
    let mut playback = Playback::Idle;
    let mut connected_at = None;
    let now = tokio::time::Instant::now();
    let mut connect_retry = tokio::time::interval_at(now + CONNECT_RETRY, CONNECT_RETRY);
    let mut discovery_retry = tokio::time::interval_at(now + DISCOVERY_RETRY, DISCOVERY_RETRY);
    connect_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    discovery_retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        if spirc.is_none()
            && task.is_none()
            && connecting.is_none()
            && reconnect
            && let Some(received) = credentials.clone()
        {
            reconnect = false;
            session.shutdown();
            session = Session::new(config.clone(), cache.clone());
            player.set_session(session.clone());
            let connect_session = session.clone();
            let connect_player = player.clone();
            let connect_mixer = mixer_handle.clone();
            let name = cfg.name.clone();
            let initial_volume = mixer.volume();
            connecting = Some(Box::pin(async move {
                let cfg = ConnectConfig {
                    name,
                    device_type: DeviceType::Speaker,
                    initial_volume,
                    ..Default::default()
                };
                let result = tokio::time::timeout(
                    CONNECT_TIMEOUT,
                    Spirc::new(
                        cfg,
                        connect_session,
                        received,
                        connect_player,
                        connect_mixer,
                    ),
                )
                .await;
                match result {
                    Ok(Ok((spirc, task))) => Ok((spirc, Box::pin(task) as Task)),
                    Ok(Err(e)) => Err(e),
                    Err(_) => Err(Error::deadline_exceeded("Spotify Connect timed out")),
                }
            }));
        }

        tokio::select! {
            credentials_event = next_discovery(&mut discovery) => match credentials_event {
                Some(received) => {
                    info!("spotify: credentials received");
                    let changed = credentials.as_ref() != Some(&received);
                    save_credentials(cache.as_ref(), &cfg.state_dir, &received);
                    credentials = Some(received);
                    retry_cached = true;
                    reconnect = true;
                    if changed && (spirc.is_some() || task.is_some() || connecting.is_some()) {
                        // Another account was selected: replace the session instead of keeping
                        // the old account connected until its socket happens to close.
                        if let Some(connected) = spirc.take() {
                            let _ = connected.shutdown();
                        }
                        connecting = None;
                        if let Some(mut active) = task.take() {
                            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, active.as_mut()).await;
                        }
                        player.stop();
                        session.shutdown();
                        connected_at = None;
                        emit(&events, &mut playback, Playback::Idle);
                    }
                }
                None => {
                    warn!("spotify: discovery stopped; advertising again in {} s", DISCOVERY_RETRY.as_secs());
                    discovery = None;
                }
            },
            result = next_connect(&mut connecting) => {
                connecting = None;
                match result {
                    Ok((connected, connected_task)) => {
                        info!("spotify: connected as '{}'", cfg.name);
                        let _ = connected.set_volume(mixer.volume());
                        spirc = Some(connected);
                        task = Some(connected_task);
                        connected_at = Some(tokio::time::Instant::now());
                    }
                    Err(e) => {
                        warn!("spotify: connect failed: {e}");
                        session.shutdown();
                        emit(&events, &mut playback, Playback::Idle);
                        if rejected_credentials(&e) {
                            warn!("spotify: credentials rejected; select the speaker in Spotify again");
                            credentials = None;
                            retry_cached = false;
                            reconnect = false;
                            forget_credentials(&cfg.state_dir);
                        } else {
                            // The interval retries a credential that worked before (or was just
                            // received); no immediate retry tight loop.
                            reconnect = false;
                        }
                    }
                }
            },
            _ = next_task(&mut task) => {
                task = None;
                if let Some(connected) = spirc.take() {
                    let _ = connected.shutdown();
                }
                player.stop();
                session.shutdown();
                emit(&events, &mut playback, Playback::Idle);
                let stable = connected_at.is_some_and(|t| t.elapsed() >= STABLE_CONNECTION);
                connected_at = None;
                retry_cached = credentials.is_some() && stable;
                reconnect = false;
                info!("spotify: connection closed");
            },
            Some(value) = volume_rx.recv() => {
                let _ = events.send(Event::Volume(volume_to_f32(value)));
            }
            event = player_events.recv() => match event {
                Some(PlayerEvent::Loading { .. }) => emit(&events, &mut playback, Playback::Loading),
                Some(PlayerEvent::Playing { .. }) => emit(&events, &mut playback, Playback::Playing),
                Some(PlayerEvent::Paused { .. }) => emit(&events, &mut playback, Playback::Paused),
                Some(PlayerEvent::Stopped { .. } | PlayerEvent::Unavailable { .. }) => {
                    emit(&events, &mut playback, Playback::Idle);
                }
                Some(_) => {}
                None => break,
            },
            command = commands.recv() => match command {
                None => break,
                Some(Cmd::Volume(value)) => {
                    let value = volume_to_u16(value);
                    mixer.report_volume(value);
                    if let Some(connected) = &spirc {
                        let _ = connected.set_volume(value);
                    }
                }
                Some(Cmd::Transport(transport)) => {
                    if let Some(connected) = &spirc {
                        let _ = match transport {
                            Transport::Play => connected.play(),
                            Transport::Pause => connected.pause(),
                            Transport::Next => connected.next(),
                            Transport::Previous => connected.prev(),
                        };
                    }
                }
            },
            _ = connect_retry.tick() => {
                if spirc.is_none() && task.is_none() && connecting.is_none() {
                    reconnect = retry_cached && credentials.is_some();
                }
            }
            _ = discovery_retry.tick() => {
                if discovery.is_none() {
                    discovery = start_discovery(&config, &cfg.name);
                }
            }
        }
    }

    player.stop();
    if let Some(connected) = spirc.take() {
        let _ = connected.shutdown();
    }
    if let Some(mut task) = task.take() {
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, task.as_mut()).await;
    }
    session.shutdown();
    if let Some(discovery) = discovery.take() {
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, discovery.shutdown()).await;
    }
}

fn emit(events: &UnboundedSender<Event>, last: &mut Playback, state: Playback) {
    if *last != state {
        *last = state;
        let _ = events.send(Event::Playback(state));
    }
}

fn open_cache(state_dir: &Path) -> Option<Cache> {
    let result = (|| -> std::io::Result<Cache> {
        std::fs::create_dir_all(state_dir)?;
        std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700))?;
        let cache = Cache::new(Some(state_dir), None, None, None).map_err(std::io::Error::other)?;
        secure_credentials(state_dir);
        Ok(cache)
    })();
    result
        .map_err(|e| warn!("spotify: credentials cache unavailable: {e}"))
        .ok()
}

/// librespot creates `credentials.json` with the process umask; keep the account token private
/// even when the cache is read by a later build or the directory mode changed.
fn secure_credentials(state_dir: &Path) {
    let path = state_dir.join("credentials.json");
    if path.exists()
        && let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
    {
        warn!("spotify: cannot secure credentials: {e}");
    }
}

fn save_credentials(cache: Option<&Cache>, state_dir: &Path, credentials: &Credentials) {
    if let Some(cache) = cache {
        create_private_credentials(state_dir);
        cache.save_credentials(credentials);
        secure_credentials(state_dir);
    }
}

fn create_private_credentials(state_dir: &Path) {
    let path = state_dir.join("credentials.json");
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(_) => {}
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            warn!("spotify: cannot create credentials securely: {e}");
        }
        Err(_) => {}
    }
}

fn start_discovery(config: &SessionConfig, name: &str) -> Option<Discovery> {
    match Discovery::builder(config.device_id.clone(), config.client_id.clone())
        .name(name.to_owned())
        .device_type(DeviceType::Speaker)
        .launch()
    {
        Ok(discovery) => {
            info!("spotify: advertising '{}' on the local network", name);
            Some(discovery)
        }
        Err(e) => {
            warn!("spotify: discovery unavailable: {e}");
            None
        }
    }
}

fn rejected_credentials(error: &Error) -> bool {
    matches!(
        error.kind,
        ErrorKind::Unauthenticated | ErrorKind::PermissionDenied
    )
}

fn forget_credentials(state_dir: &Path) {
    let path = state_dir.join("credentials.json");
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        warn!("spotify: cannot remove rejected credentials: {e}");
    }
}

async fn next_discovery(discovery: &mut Option<Discovery>) -> Option<Credentials> {
    match discovery {
        Some(discovery) => discovery.next().await,
        None => pending().await,
    }
}

async fn next_connect(connecting: &mut Option<Connecting>) -> Result<(Spirc, Task), Error> {
    match connecting {
        Some(connecting) => connecting.as_mut().await,
        None => pending().await,
    }
}

async fn next_task(task: &mut Option<Task>) {
    match task {
        Some(task) => task.as_mut().await,
        None => pending().await,
    }
}
