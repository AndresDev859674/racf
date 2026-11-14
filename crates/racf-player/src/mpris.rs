//! Optional MPRIS2 service.
//!
//! The service publishes playback state: status, metadata, length, and
//! position. It intentionally exposes no transport methods. The interactive
//! `:` CLI can pause, seek, and loop locally, while this D-Bus interface stays
//! a status-only view of what the player is doing.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use racf_core::{Document, Timeline};

/// Status shared between the transport loop and the MPRIS thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackStatus {
    Stopped,
    Playing,
    Paused,
}

impl PlaybackStatus {
    const fn code(self) -> u8 {
        match self {
            PlaybackStatus::Stopped => 0,
            PlaybackStatus::Playing => 1,
            PlaybackStatus::Paused => 2,
        }
    }

    const fn from_code(code: u8) -> Self {
        match code {
            1 => PlaybackStatus::Playing,
            2 => PlaybackStatus::Paused,
            _ => PlaybackStatus::Stopped,
        }
    }
}

struct SharedState {
    position_ms: AtomicU64,
    status: AtomicU8,
    stop: AtomicBool,
}

impl SharedState {
    fn new() -> Self {
        Self {
            position_ms: AtomicU64::new(0),
            status: AtomicU8::new(PlaybackStatus::Stopped.code()),
            stop: AtomicBool::new(false),
        }
    }
}

/// Keeps the MPRIS service alive and publishes transport state to it.
///
/// Dropping the handle leaves the service running until the process exits,
/// which is fine for a player that owns the terminal. [`MprisHandle::stop`]
/// marks playback as stopped first.
pub struct MprisHandle {
    state: Arc<SharedState>,
}

impl MprisHandle {
    fn disabled() -> Self {
        Self {
            state: Arc::new(SharedState::new()),
        }
    }

    /// Updates the position reported over D-Bus.
    pub fn set_position(&self, position_ms: u64) {
        self.state.position_ms.store(position_ms, Ordering::Relaxed);
    }

    pub fn set_status(&self, status: PlaybackStatus) {
        self.state.status.store(status.code(), Ordering::Relaxed);
    }

    pub fn status(&self) -> PlaybackStatus {
        PlaybackStatus::from_code(self.state.status.load(Ordering::Relaxed))
    }

    /// Marks the player as stopped and shuts the service thread down.
    pub fn stop(&self) {
        self.set_status(PlaybackStatus::Stopped);
        self.state.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts a best-effort MPRIS service.
///
/// Failure to reach the session bus is reported once and never prevents audio
/// playback.
pub fn start(
    document: &Document,
    timeline: &Timeline,
    source: &Path,
    enabled: bool,
) -> MprisHandle {
    if !enabled || std::env::var_os("RACF_NO_MPRIS").is_some() {
        return MprisHandle::disabled();
    }

    let data = Data {
        title: document
            .metadata
            .title
            .clone()
            .or_else(|| {
                source
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "RACF document".to_string()),
        description: document.metadata.description.clone(),
        artist: document.metadata.artist.clone(),
        album: document.metadata.album.clone(),
        length_ms: timeline.end_ms,
    };

    let state = Arc::new(SharedState::new());
    state
        .status
        .store(PlaybackStatus::Playing.code(), Ordering::Relaxed);
    let thread_state = Arc::clone(&state);

    let result = std::thread::Builder::new()
        .name("racf-mpris".to_string())
        .spawn(move || run(data, thread_state));

    if let Err(error) = result {
        eprintln!("[RACF] MPRIS could not start: {error}");
    }

    MprisHandle { state }
}

#[derive(Clone, Debug)]
struct Data {
    title: String,
    description: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    length_ms: u64,
}

#[cfg(target_os = "linux")]
fn run(data: Data, state: Arc<SharedState>) {
    use std::time::Duration;

    use mpris_server::{Metadata, PlaybackStatus as MprisStatus, Player, Time};

    fn to_mpris(status: PlaybackStatus) -> MprisStatus {
        match status {
            PlaybackStatus::Stopped => MprisStatus::Stopped,
            PlaybackStatus::Playing => MprisStatus::Playing,
            PlaybackStatus::Paused => MprisStatus::Paused,
        }
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[RACF] MPRIS runtime could not start: {error}");
            return;
        }
    };

    let local = tokio::task::LocalSet::new();
    let result = local.block_on(&runtime, async move {
        let mut metadata = Metadata::builder()
            .title(data.title.clone())
            .length(Time::from_millis(data.length_ms as i64));

        if let Some(artist) = data.artist.clone() {
            metadata = metadata.artist([artist]);
        }
        if let Some(album) = data.album.clone() {
            metadata = metadata.album(album);
        }
        if let Some(description) = data.description.clone() {
            metadata = metadata.comment([description]);
        }

        let player = Player::builder("racf")
            .identity("RACF Player")
            .desktop_entry("racf-player")
            .supported_uri_schemes(["file"])
            .supported_mime_types(["application/x-racf+xml", "text/xml"])
            .can_control(true)
            .can_play(false)
            .can_pause(false)
            .can_seek(false)
            .can_go_next(false)
            .can_go_previous(false)
            .playback_status(MprisStatus::Playing)
            .metadata(metadata.build())
            .build()
            .await?;

        tokio::task::spawn_local(player.run());

        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        let mut reported_status = PlaybackStatus::Playing;
        let mut reported_position = u64::MAX;

        loop {
            ticker.tick().await;

            if state.stop.load(Ordering::Relaxed) {
                break;
            }

            let status = PlaybackStatus::from_code(state.status.load(Ordering::Relaxed));
            if status != reported_status {
                player.set_playback_status(to_mpris(status)).await?;
                reported_status = status;
            }

            let position = state.position_ms.load(Ordering::Relaxed);
            if status != PlaybackStatus::Stopped && position != reported_position {
                player.set_position(Time::from_millis(position as i64));
                reported_position = position;
            }
        }

        if reported_status != PlaybackStatus::Stopped {
            player.set_playback_status(MprisStatus::Stopped).await?;
        }

        Ok::<(), mpris_server::zbus::Error>(())
    });

    if let Err(error) = result {
        eprintln!("[RACF] MPRIS unavailable: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
fn run(_data: Data, _state: Arc<SharedState>) {}
