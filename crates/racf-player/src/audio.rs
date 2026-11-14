use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use racf_core::{Document, ScheduledAction, Timeline};

use crate::error::PlayerError;
use crate::mixer::MasterMixer;

/// Opens the default output device and plays a compiled timeline.
///
/// `source` is used as a fallback title for the MPRIS service. Set
/// `enable_mpris` to `false` to run without publishing to D-Bus.
pub fn play(
    document: &Document,
    timeline: &Timeline,
    source: &Path,
    enable_mpris: bool,
) -> Result<(), PlayerError> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or(PlayerError::NoOutputDevice)?;
    let supported = device
        .default_output_config()
        .map_err(|error| PlayerError::Audio(error.to_string()))?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let sample_rate = config.sample_rate.0 as f32;
    let device_channels = config.channels as usize;

    let mixer = Arc::new(Mutex::new(MasterMixer::new(document, sample_rate)));
    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::F64 => build_stream::<f64>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I8 => build_stream::<i8>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I32 => build_stream::<i32>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::I64 => build_stream::<i64>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U8 => build_stream::<u8>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U32 => build_stream::<u32>(&device, &config, Arc::clone(&mixer)),
        SampleFormat::U64 => build_stream::<u64>(&device, &config, Arc::clone(&mixer)),
        other => {
            return Err(PlayerError::UnsupportedSampleFormat(format!("{other:?}")));
        }
    }?;

    stream
        .play()
        .map_err(|error| PlayerError::Audio(error.to_string()))?;

    println!(
        "[RACF] Output: {} | {} Hz | {} channel(s) | layout: {}",
        device
            .name()
            .unwrap_or_else(|_| "unknown device".to_string()),
        config.sample_rate.0,
        device_channels,
        mixer.lock().unwrap().layout()
    );

    let mpris = crate::mpris::start(document, timeline, source, enable_mpris);
    let start = Instant::now();

    for event in &timeline.events {
        let target = Duration::from_millis(event.timestamp_ms);
        let elapsed = start.elapsed();
        if target > elapsed {
            std::thread::sleep(target - elapsed);
        }

        let mut guard = mixer.lock().unwrap();
        match &event.action {
            ScheduledAction::Play(note) => guard.push_note(note),
            ScheduledAction::PlayAudio(audio) => guard.push_audio(audio),
            ScheduledAction::SetSeparation { active, target } => {
                guard.set_separation(*active, target.clone());
                if *active {
                    println!("[RACF] Stem separation on -> isolated: {target:?}");
                } else {
                    println!("[RACF] Stem separation off -> normal mix restored");
                }
            }
        }
    }

    // Let the final voices ring out before the stream is dropped.
    let tail_end = Duration::from_millis(timeline.end_ms) + Duration::from_millis(1500);
    let elapsed = start.elapsed();
    if tail_end > elapsed {
        std::thread::sleep(tail_end - elapsed);
    }

    mpris.stop();
    drop(stream);
    Ok(())
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    mixer: Arc<Mutex<MasterMixer>>,
) -> Result<cpal::Stream, PlayerError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut scratch = Vec::<f32>::new();

    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                if scratch.len() != data.len() {
                    scratch.resize(data.len(), 0.0);
                }

                mixer.lock().unwrap().render(&mut scratch, channels);

                for (output, sample) in data.iter_mut().zip(scratch.iter()) {
                    *output = T::from_sample(*sample);
                }
            },
            |error| eprintln!("[RACF] Audio stream error: {error}"),
            None,
        )
        .map_err(|error| PlayerError::Audio(error.to_string()))
}
