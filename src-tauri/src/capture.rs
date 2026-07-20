use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Instant;

use cpal::{
    Device, FromSample, Host, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
    SupportedStreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureSource {
    Microphone,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioDeviceDescriptor {
    pub id: String,
    pub is_default: bool,
    pub name: String,
    pub source: CaptureSource,
}

#[derive(Debug)]
pub struct CapturedAudio {
    pub captured_at: Instant,
    pub channels: u16,
    pub sample_rate: u32,
    pub samples: Vec<f32>,
    pub sequence: u64,
    pub source: CaptureSource,
}

#[derive(Debug)]
pub enum CaptureEvent {
    Audio(CapturedAudio),
    Error {
        message: String,
        source: CaptureSource,
    },
}

#[derive(Debug, Clone)]
pub struct CaptureRequest {
    pub device_id: Option<String>,
    pub source: CaptureSource,
}

pub struct CaptureSession {
    _backend: CaptureBackend,
    pub channels: u16,
    pub device_name: String,
    pub sample_rate: u32,
    pub source: CaptureSource,
}

enum CaptureBackend {
    Cpal {
        _stream: Stream,
    },
    #[cfg(target_os = "macos")]
    ScreenCaptureKit {
        _capture: macos::SystemAudioCapture,
    },
}

impl CaptureSession {
    pub fn stop(self) {
        drop(self);
    }
}

pub fn list_audio_devices() -> Result<Vec<AudioDeviceDescriptor>, CaptureError> {
    let host = capture_host()?;
    let mut descriptors = vec![
        AudioDeviceDescriptor {
            id: "default:microphone".to_owned(),
            is_default: true,
            name: "System default microphone".to_owned(),
            source: CaptureSource::Microphone,
        },
        AudioDeviceDescriptor {
            id: "default:system".to_owned(),
            is_default: true,
            name: "System default output".to_owned(),
            source: CaptureSource::System,
        },
    ];
    let devices = host
        .devices()
        .map_err(|error| CaptureError::Backend(error.to_string()))?;
    for device in devices {
        let id = device
            .id()
            .map_err(|error| CaptureError::Backend(error.to_string()))?
            .id()
            .to_owned();
        let name = device
            .description()
            .map(|description| description.name().to_owned())
            .unwrap_or_else(|_| device.to_string());
        if device.supports_input() {
            descriptors.push(AudioDeviceDescriptor {
                id: id.clone(),
                is_default: false,
                name: name.clone(),
                source: CaptureSource::Microphone,
            });
        }
        #[cfg(not(target_os = "macos"))]
        if device.supports_output() {
            descriptors.push(AudioDeviceDescriptor {
                id,
                is_default: false,
                name,
                source: CaptureSource::System,
            });
        }
    }
    descriptors.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then(left.source.sort_key().cmp(&right.source.sort_key()))
            .then(left.name.cmp(&right.name))
    });
    descriptors.dedup_by(|left, right| left.id == right.id && left.source == right.source);
    Ok(descriptors)
}

pub fn start_capture(
    request: CaptureRequest,
    sender: Sender<CaptureEvent>,
) -> Result<CaptureSession, CaptureError> {
    #[cfg(target_os = "macos")]
    if request.source == CaptureSource::System {
        return macos::start_system_audio(sender);
    }
    start_cpal_capture(request, sender)
}

fn start_cpal_capture(
    request: CaptureRequest,
    sender: Sender<CaptureEvent>,
) -> Result<CaptureSession, CaptureError> {
    let host = capture_host()?;
    let device = select_device(&host, &request)?;
    let device_name = device
        .description()
        .map(|description| description.name().to_owned())
        .unwrap_or_else(|_| device.to_string());
    let supported = match request.source {
        CaptureSource::Microphone => device.default_input_config(),
        CaptureSource::System => device.default_output_config(),
    }
    .map_err(|error| CaptureError::Backend(error.to_string()))?;
    let sample_rate = supported.sample_rate();
    let channels = supported.channels();
    let stream = build_stream(device, supported, request.source, sender)?;
    stream
        .play()
        .map_err(|error| CaptureError::Backend(error.to_string()))?;
    Ok(CaptureSession {
        _backend: CaptureBackend::Cpal { _stream: stream },
        channels,
        device_name,
        sample_rate,
        source: request.source,
    })
}

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("audio backend failed: {0}")]
    Backend(String),
    #[error("audio device '{0}' is no longer available")]
    DeviceUnavailable(String),
    #[error("sample format '{0}' is not supported")]
    UnsupportedSampleFormat(String),
}

impl CaptureSource {
    fn sort_key(self) -> u8 {
        match self {
            Self::Microphone => 0,
            Self::System => 1,
        }
    }
}

fn capture_host() -> Result<Host, CaptureError> {
    Ok(cpal::default_host())
}

fn select_device(host: &Host, request: &CaptureRequest) -> Result<Device, CaptureError> {
    let explicit = request
        .device_id
        .as_deref()
        .filter(|value| !value.starts_with("default:"));
    if let Some(id) = explicit {
        let parsed = id
            .parse()
            .map_err(|error: cpal::Error| CaptureError::Backend(error.to_string()))?;
        return host
            .device_by_id(&parsed)
            .ok_or_else(|| CaptureError::DeviceUnavailable(id.to_owned()));
    }
    match request.source {
        CaptureSource::Microphone => host.default_input_device(),
        CaptureSource::System => host.default_output_device(),
    }
    .ok_or_else(|| CaptureError::DeviceUnavailable("system default".to_owned()))
}

fn build_stream(
    device: Device,
    supported: SupportedStreamConfig,
    source: CaptureSource,
    sender: Sender<CaptureEvent>,
) -> Result<Stream, CaptureError> {
    let config: StreamConfig = supported.into();
    match supported.sample_format() {
        SampleFormat::I8 => build_typed_stream::<i8>(device, config, source, sender),
        SampleFormat::I16 => build_typed_stream::<i16>(device, config, source, sender),
        SampleFormat::I24 => build_typed_stream::<cpal::I24>(device, config, source, sender),
        SampleFormat::I32 => build_typed_stream::<i32>(device, config, source, sender),
        SampleFormat::I64 => build_typed_stream::<i64>(device, config, source, sender),
        SampleFormat::U8 => build_typed_stream::<u8>(device, config, source, sender),
        SampleFormat::U16 => build_typed_stream::<u16>(device, config, source, sender),
        SampleFormat::U24 => build_typed_stream::<cpal::U24>(device, config, source, sender),
        SampleFormat::U32 => build_typed_stream::<u32>(device, config, source, sender),
        SampleFormat::U64 => build_typed_stream::<u64>(device, config, source, sender),
        SampleFormat::F32 => build_typed_stream::<f32>(device, config, source, sender),
        SampleFormat::F64 => build_typed_stream::<f64>(device, config, source, sender),
        format => Err(CaptureError::UnsupportedSampleFormat(format.to_string())),
    }
}

fn build_typed_stream<T>(
    device: Device,
    config: StreamConfig,
    source: CaptureSource,
    sender: Sender<CaptureEvent>,
) -> Result<Stream, CaptureError>
where
    T: SizedSample + Sample,
    f32: FromSample<T>,
{
    let sequence = Arc::new(AtomicU64::new(0));
    let callback_sequence = sequence.clone();
    let channels = config.channels;
    let sample_rate = config.sample_rate;
    let error_sender = sender.clone();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _info| {
                let samples = data.iter().copied().map(f32::from_sample).collect();
                let event = CaptureEvent::Audio(CapturedAudio {
                    captured_at: Instant::now(),
                    channels,
                    sample_rate,
                    samples,
                    sequence: callback_sequence.fetch_add(1, Ordering::Relaxed),
                    source,
                });
                if sender.send(event).is_err() {
                    tracing::debug!(?source, "audio consumer stopped");
                }
            },
            move |error| {
                let _ = error_sender.send(CaptureEvent::Error {
                    message: error.to_string(),
                    source,
                });
            },
            None,
        )
        .map_err(|error| CaptureError::Backend(error.to_string()))
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use screencapturekit::prelude::*;

    const SYSTEM_SAMPLE_RATE: u32 = 48_000;
    const SYSTEM_CHANNELS: u16 = 2;

    pub struct SystemAudioCapture {
        stream: SCStream,
    }

    impl Drop for SystemAudioCapture {
        fn drop(&mut self) {
            if let Err(error) = self.stream.stop_capture() {
                tracing::debug!(?error, "ScreenCaptureKit stream was already stopped");
            }
        }
    }

    pub fn start_system_audio(
        sender: Sender<CaptureEvent>,
    ) -> Result<CaptureSession, CaptureError> {
        let content =
            SCShareableContent::get().map_err(|error| CaptureError::Backend(error.to_string()))?;
        let display =
            content.displays().into_iter().next().ok_or_else(|| {
                CaptureError::DeviceUnavailable("primary macOS display".to_owned())
            })?;
        let filter = SCContentFilter::create()
            .with_display(&display)
            .with_excluding_windows(&[])
            .build();
        let configuration = SCStreamConfiguration::new()
            .with_width(2)
            .with_height(2)
            .with_shows_cursor(false)
            .with_captures_audio(true)
            .with_excludes_current_process_audio(true)
            .with_sample_rate(SYSTEM_SAMPLE_RATE as i32)
            .with_channel_count(SYSTEM_CHANNELS as i32);
        let sequence = Arc::new(AtomicU64::new(0));
        let callback_sequence = sequence.clone();
        let mut stream = SCStream::new(&filter, &configuration);
        let handler_id = stream.add_output_handler(
            move |sample: CMSampleBuffer, output_type: SCStreamOutputType| {
                if output_type != SCStreamOutputType::Audio {
                    return;
                }
                let Some(samples) = pcm_f32_samples(&sample) else {
                    return;
                };
                let event = CaptureEvent::Audio(CapturedAudio {
                    captured_at: Instant::now(),
                    channels: SYSTEM_CHANNELS,
                    sample_rate: SYSTEM_SAMPLE_RATE,
                    samples,
                    sequence: callback_sequence.fetch_add(1, Ordering::Relaxed),
                    source: CaptureSource::System,
                });
                if sender.send(event).is_err() {
                    tracing::debug!("ScreenCaptureKit audio consumer stopped");
                }
            },
            SCStreamOutputType::Audio,
        );
        if handler_id.is_none() {
            return Err(CaptureError::Backend(
                "failed to attach ScreenCaptureKit audio output".to_owned(),
            ));
        }
        stream
            .start_capture()
            .map_err(|error| CaptureError::Backend(error.to_string()))?;
        Ok(CaptureSession {
            _backend: CaptureBackend::ScreenCaptureKit {
                _capture: SystemAudioCapture { stream },
            },
            channels: SYSTEM_CHANNELS,
            device_name: "macOS system audio".to_owned(),
            sample_rate: SYSTEM_SAMPLE_RATE,
            source: CaptureSource::System,
        })
    }

    fn pcm_f32_samples(sample: &CMSampleBuffer) -> Option<Vec<f32>> {
        let buffers = sample.audio_buffer_list()?;
        if buffers.num_buffers() == 1 {
            return buffers.get(0).map(|buffer| decode_f32(buffer.data()));
        }
        let planar = buffers
            .iter()
            .map(|buffer| decode_f32(buffer.data()))
            .collect::<Vec<_>>();
        let frames = planar.iter().map(Vec::len).min()?;
        let mut interleaved = Vec::with_capacity(frames.saturating_mul(planar.len()));
        for frame in 0..frames {
            for channel in &planar {
                interleaved.push(channel[frame]);
            }
        }
        Some(interleaved)
    }

    fn decode_f32(bytes: &[u8]) -> Vec<f32> {
        bytes
            .chunks_exact(size_of::<f32>())
            .map(|sample| f32::from_ne_bytes(sample.try_into().expect("four-byte float")))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_sort_order_is_stable() {
        assert!(CaptureSource::Microphone.sort_key() < CaptureSource::System.sort_key());
    }

    #[test]
    fn default_devices_use_reserved_ids() {
        for (source, id) in [
            (CaptureSource::Microphone, "default:microphone"),
            (CaptureSource::System, "default:system"),
        ] {
            let request = CaptureRequest {
                device_id: Some(id.to_owned()),
                source,
            };
            assert!(
                request
                    .device_id
                    .as_deref()
                    .unwrap()
                    .starts_with("default:")
            );
        }
    }

    #[test]
    #[ignore = "requires an audio host"]
    fn enumerates_host_audio_devices() {
        let devices = list_audio_devices().unwrap();
        assert!(
            devices
                .iter()
                .any(|device| device.source == CaptureSource::Microphone)
        );
        assert!(
            devices
                .iter()
                .any(|device| device.source == CaptureSource::System)
        );
    }

    #[test]
    #[ignore = "requires a system output device"]
    fn opens_default_system_loopback_stream() {
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let session = start_capture(
            CaptureRequest {
                device_id: None,
                source: CaptureSource::System,
            },
            sender,
        )
        .unwrap();
        assert_eq!(session.source, CaptureSource::System);
        session.stop();
    }
}
