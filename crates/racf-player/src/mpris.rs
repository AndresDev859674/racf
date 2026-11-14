//! Optional MPRIS2 service.
//!
//! The service publishes what is playing (status, title, description, artist,
//! album, length, position). It intentionally exposes no transport controls:
//! `racf-player` plays a file once from the command line, so pausing or seeking
//! has no meaning.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use racf_core::{Document, Timeline};

/// Keeps the MPRIS service alive and tells it when playback has finished.
///
/// Dropping the handle leaves the service running until the process exits,
/// which is fine for a one-shot CLI player. Calling [`MprisHandle::stop`] marks
/// the player as stopped first.
pub struct MprisHandle {
    stop: Arc<AtomicBool>,
}

impl MprisHandle {
    fn disabled() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(true)),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
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

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);

    let result = std::thread::Builder::new()
        .name("racf-mpris".to_string())
        .spawn(move || run(data, thread_stop));

    if let Err(error) = result {
        eprintln!("[RACF] MPRIS could not start: {error}");
        return MprisHandle::disabled();
    }

    MprisHandle { stop }
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
fn run(data: Data, stop: Arc<AtomicBool>) {
    use std::time::Duration;

    use mpris_server::{Metadata, PlaybackStatus, Player, Time};

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
            .playback_status(PlaybackStatus::Playing)
            .metadata(metadata.build())
            .build()
            .await?;

        tokio::task::spawn_local(player.run());

        let started = std::time::Instant::now();
        let mut ticker = tokio::time::interval(Duration::from_millis(500));

        loop {
            ticker.tick().await;

            if stop.load(Ordering::Relaxed) {
                player.set_playback_status(PlaybackStatus::Stopped).await?;
                break;
            }

            let elapsed = started.elapsed();
            if elapsed.as_millis() as u64 >= data.length_ms {
                player.set_position(Time::from_micros(elapsed.as_micros() as i64));
                player.set_playback_status(PlaybackStatus::Stopped).await?;
                break;
            }

            player.set_position(Time::from_micros(elapsed.as_micros() as i64));
        }

        Ok::<(), mpris_server::zbus::Error>(())
    });

    if let Err(error) = result {
        eprintln!("[RACF] MPRIS unavailable: {error}");
    }
}

#[cfg(not(target_os = "linux"))]
fn run(_data: Data, _stop: Arc<AtomicBool>) {}
