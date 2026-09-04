//! Device enumeration and streaming, one API over WASAPI and ALSA.
//!
//! Everything above this layer works in mono f32 at 48 kHz. Channel count is
//! reconciled here: multi-channel input is downmixed, mono output is fanned out
//! to every channel the device wants.
//!
//! Sample rate is a preference, not a demand: the handle reports what the
//! device actually gave, and the engine resamples the difference away.

#[cfg(target_os = "linux")]
pub mod pipewire_source;
pub mod sink;

use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat, SampleRate, StreamConfig, SupportedStreamConfig};
use mb_i18n::{t, t1, t2, Key as K};

pub use cpal::Stream;
pub use sink::{
    latency_hint, looks_like_virtual_cable, open_sink, Sink, DISPLAY_NAME, VIRTUAL_SINK_HINTS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Input,
    Output,
}

impl Direction {
    fn label(self) -> &'static str {
        match self {
            Direction::Input => t(K::ErrDirIn),
            Direction::Output => t(K::ErrDirOut),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub index: usize,
    pub name: String,
    pub is_default: bool,
    /// None when the device refuses to describe itself, which happens with
    /// half-disconnected USB gear.
    pub default_sample_rate: Option<u32>,
    pub channels: Option<u16>,
}

pub fn list(dir: Direction) -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = match dir {
        Direction::Input => host.default_input_device().and_then(|d| d.name().ok()),
        Direction::Output => host.default_output_device().and_then(|d| d.name().ok()),
    };

    let devices: Vec<Device> = match dir {
        Direction::Input => host.input_devices()?.collect(),
        Direction::Output => host.output_devices()?.collect(),
    };

    let mut out = Vec::new();
    for (index, device) in devices.into_iter().enumerate() {
        let name = device
            .name()
            .unwrap_or_else(|_| format!("<urządzenie {index} bez nazwy>"));
        let cfg = match dir {
            Direction::Input => device.default_input_config().ok(),
            Direction::Output => device.default_output_config().ok(),
        };
        out.push(DeviceInfo {
            index,
            is_default: Some(&name) == default_name.as_ref(),
            default_sample_rate: cfg.as_ref().map(|c| c.sample_rate().0),
            channels: cfg.as_ref().map(|c| c.channels()),
            name,
        });
    }
    Ok(out)
}

/// Resolve a user-supplied selector to a device.
///
/// Accepted forms, in this order:
///   `default`     the system default
///   `@3`          index from `micbridge devices`
///   `yeti`        case-insensitive substring of the name
///
/// Substring matching is what keeps the selector stable across reboots: index
/// numbers move when USB gear is replugged, names do not.
pub fn find(dir: Direction, selector: &str) -> Result<Device> {
    let host = cpal::default_host();
    let selector = selector.trim();

    if selector.eq_ignore_ascii_case("default") {
        return match dir {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        }
        .ok_or_else(|| anyhow!("{}", t1(K::ErrNoDefault, dir.label())));
    }

    let devices: Vec<Device> = match dir {
        Direction::Input => host.input_devices()?.collect(),
        Direction::Output => host.output_devices()?.collect(),
    };
    let names: Vec<String> = devices
        .iter()
        .map(|d| d.name().unwrap_or_default())
        .collect();

    if let Some(rest) = selector.strip_prefix('@') {
        let idx: usize = rest
            .parse()
            .with_context(|| format!("`{selector}` nie jest indeksem urządzenia"))?;
        return devices
            .into_iter()
            .nth(idx)
            .ok_or_else(|| anyhow!("{}", t2(K::ErrNoDeviceIdx, dir.label(), idx)));
    }

    let needle = selector.to_lowercase();
    let matches: Vec<usize> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.to_lowercase().contains(&needle))
        .map(|(i, _)| i)
        .collect();

    match matches.as_slice() {
        [] => bail!(
            "żadne urządzenie {} nie pasuje do `{selector}`. Dostępne:\n  {}",
            dir.label(),
            names.join("\n  ")
        ),
        [i] => Ok(devices.into_iter().nth(*i).expect("index from filter")),
        many => bail!(
            "`{selector}` pasuje do {} urządzeń, doprecyzuj:\n  {}",
            many.len(),
            many.iter()
                .map(|&i| names[i].clone())
                .collect::<Vec<_>>()
                .join("\n  ")
        ),
    }
}

/// Pick a device configuration, preferring `rate` but never insisting on it.
///
/// Milestone 1 refused anything but 48 kHz. Now the engine resamples at its
/// edges, so a device that only offers 44.1 kHz is a routine case rather than
/// an error — and telling a user to go change their Windows sound settings was
/// never an acceptable answer.
///
/// Preference order: the requested rate, then fewest channels (mono input
/// costs no downmix), then f32 over i16.
fn config_for(device: &Device, dir: Direction, rate: u32) -> Result<SupportedStreamConfig> {
    let ranges: Vec<_> = match dir {
        Direction::Input => device.supported_input_configs()?.collect(),
        Direction::Output => device.supported_output_configs()?.collect(),
    };

    let mut usable: Vec<_> = ranges
        .into_iter()
        .filter(|r| matches!(r.sample_format(), SampleFormat::F32 | SampleFormat::I16))
        .collect();

    if usable.is_empty() {
        let name = device.name().unwrap_or_else(|_| "<bez nazwy>".into());
        bail!("{}", t1(K::ErrNoFormat, name));
    }

    usable.sort_by_key(|r| {
        (
            // Ranges that cover the requested rate come first.
            u8::from(!(r.min_sample_rate().0 <= rate && rate <= r.max_sample_rate().0)),
            r.channels(),
            u8::from(r.sample_format() != SampleFormat::F32),
        )
    });

    let chosen = usable.into_iter().next().expect("checked non-empty");
    let picked = rate.clamp(chosen.min_sample_rate().0, chosen.max_sample_rate().0);
    if picked != rate {
        tracing::info!(
            device = %device.name().unwrap_or_default(),
            wanted = rate,
            using = picked,
            "urządzenie nie ma żądanej częstotliwości — resampling po stronie silnika"
        );
    }
    Ok(chosen.with_sample_rate(SampleRate(picked)))
}

pub struct CaptureHandle {
    /// Dropping this stops the stream, so the caller must keep it alive.
    _stream: Stream,
    pub device_name: String,
    pub channels: u16,
    pub sample_rate: u32,
}

/// Start capturing, calling `on_mono` with downmixed f32 samples in [-1, 1].
///
/// `rate` is a preference; check `CaptureHandle::sample_rate` for what the
/// device actually gave and resample if it differs.
///
/// `on_mono` runs on the audio thread: no allocation, no locks, no I/O.
pub fn start_capture<F>(selector: &str, rate: u32, mut on_mono: F) -> Result<CaptureHandle>
where
    F: FnMut(&[f32]) + Send + 'static,
{
    let device = find(Direction::Input, selector)?;
    let device_name = device.name().unwrap_or_else(|_| "<bez nazwy>".into());
    let supported = config_for(&device, Direction::Input, rate)?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;

    // Scratch space owned by the closure, sized once, reused every callback.
    let mut mono = vec![0f32; 4096];

    let err_fn = |e| tracing::error!(error = %e, "błąd strumienia wejściowego");

    macro_rules! build {
        ($sample:ty, $to_f32:expr) => {
            device.build_input_stream(
                &config,
                move |data: &[$sample], _: &cpal::InputCallbackInfo| {
                    let frames = data.len() / channels;
                    if mono.len() < frames {
                        // Only grows on the first oversized callback.
                        mono.resize(frames, 0.0);
                    }
                    let gain = 1.0 / channels as f32;
                    for (i, chunk) in data.chunks_exact(channels).enumerate() {
                        let sum: f32 = chunk.iter().map(|&s| $to_f32(s)).sum();
                        mono[i] = sum * gain;
                    }
                    on_mono(&mono[..frames]);
                },
                err_fn,
                None,
            )?
        };
    }

    let stream = match format {
        SampleFormat::F32 => build!(f32, |s: f32| s),
        SampleFormat::I16 => build!(i16, |s: i16| s as f32 / 32768.0),
        other => bail!("nieobsługiwany format próbek: {other:?}"),
    };

    stream.play()?;
    Ok(CaptureHandle {
        _stream: stream,
        device_name,
        channels: config.channels,
        sample_rate: config.sample_rate.0,
    })
}

pub struct PlaybackHandle {
    _stream: Stream,
    pub device_name: String,
    pub channels: u16,
    pub sample_rate: u32,
}

/// Start playback, asking `fill_mono` for f32 samples in [-1, 1].
///
/// `rate` is a preference; check `PlaybackHandle::sample_rate` for what the
/// device actually gave.
///
/// The callback must fill the whole slice; silence is the correct answer when
/// there is nothing to play. It runs on the audio thread.
pub fn start_playback<F>(selector: &str, rate: u32, mut fill_mono: F) -> Result<PlaybackHandle>
where
    F: FnMut(&mut [f32]) + Send + 'static,
{
    let device = find(Direction::Output, selector)?;
    let device_name = device.name().unwrap_or_else(|_| "<bez nazwy>".into());
    let supported = config_for(&device, Direction::Output, rate)?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = config.channels as usize;

    let mut mono = vec![0f32; 4096];
    let err_fn = |e| tracing::error!(error = %e, "błąd strumienia wyjściowego");

    macro_rules! build {
        ($sample:ty, $from_f32:expr) => {
            device.build_output_stream(
                &config,
                move |data: &mut [$sample], _: &cpal::OutputCallbackInfo| {
                    let frames = data.len() / channels;
                    if mono.len() < frames {
                        mono.resize(frames, 0.0);
                    }
                    fill_mono(&mut mono[..frames]);
                    for (chunk, &s) in data.chunks_exact_mut(channels).zip(mono.iter()) {
                        let v = $from_f32(s);
                        chunk.fill(v);
                    }
                },
                err_fn,
                None,
            )?
        };
    }

    let stream = match format {
        SampleFormat::F32 => build!(f32, |s: f32| s),
        SampleFormat::I16 => build!(i16, |s: f32| (s.clamp(-1.0, 1.0) * 32767.0) as i16),
        other => bail!("nieobsługiwany format próbek: {other:?}"),
    };

    stream.play()?;
    Ok(PlaybackHandle {
        _stream: stream,
        device_name,
        channels: config.channels,
        sample_rate: config.sample_rate.0,
    })
}

/// Peak level of a block, in dBFS. Drives the meter that tells the user
/// whether the problem is the microphone or the link.
pub fn peak_dbfs(samples: &[f32]) -> f32 {
    let peak = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    if peak <= 1e-6 {
        -120.0
    } else {
        20.0 * peak.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_is_zero_dbfs() {
        assert!((peak_dbfs(&[1.0, -0.5]) - 0.0).abs() < 0.01);
    }

    #[test]
    fn half_scale_is_about_minus_six() {
        assert!((peak_dbfs(&[0.5]) + 6.02).abs() < 0.05);
    }

    #[test]
    fn silence_is_floored_not_infinite() {
        assert_eq!(peak_dbfs(&[0.0; 16]), -120.0);
    }
}
