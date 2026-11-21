use anyhow::{anyhow, bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
};
use crossbeam_queue::ArrayQueue;
use serde::Serialize;
use std::{
    ffi::{c_char, c_void, CStr, CString},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed},
        mpsc, Arc,
    },
};
use tokio::sync::oneshot;
use voxa::pcm::Playback;

extern "C" {
    fn voxa_sources() -> *mut c_char;
    fn voxa_free_string(value: *mut c_char);
    fn voxa_microphone_permission() -> i32;
    fn voxa_request_microphone(callback: extern "C" fn(bool, *mut c_void), context: *mut c_void);
    fn voxa_start_tap(
        source: *const c_char,
        callback: extern "C" fn(*const f32, usize, *mut c_void),
        context: *mut c_void,
        rate: *mut f64,
        error: *mut i32,
    ) -> *mut c_void;
    fn voxa_stop_tap(tap: *mut c_void);
}

#[derive(Serialize, serde::Deserialize)]
pub struct Source {
    id: String,
    name: String,
}

#[derive(Serialize)]
pub struct Devices {
    sources: Vec<Source>,
    microphones: Vec<String>,
    speakers: Vec<String>,
    blackhole: Vec<String>,
}

pub fn devices() -> Result<Devices> {
    let raw = unsafe { voxa_sources() };
    let mut sources: Vec<Source> = if raw.is_null() {
        Vec::new()
    } else {
        let parsed = serde_json::from_slice(unsafe { CStr::from_ptr(raw) }.to_bytes());
        unsafe { voxa_free_string(raw) };
        parsed.context("Không đọc được danh sách nguồn Core Audio.")?
    };
    sources.sort_by_cached_key(|source| source.name.to_lowercase());
    let host = cpal::default_host();
    let mut microphones: Vec<String> = host
        .input_devices()?
        .filter_map(|device| device.name().ok())
        .filter(|name| !is_blackhole(name))
        .collect();
    let mut speakers = Vec::new();
    let mut blackhole = Vec::new();
    for name in host
        .output_devices()?
        .filter_map(|device| device.name().ok())
    {
        if is_blackhole(&name) {
            blackhole.push(name);
        } else if name != "Voxa Private Capture" {
            speakers.push(name);
        }
    }
    for names in [&mut microphones, &mut speakers, &mut blackhole] {
        names.sort_unstable();
        names.dedup();
    }
    Ok(Devices {
        sources,
        microphones,
        speakers,
        blackhole,
    })
}

fn is_blackhole(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("blackhole")
}

pub async fn microphone_permission() -> Result<()> {
    match unsafe { voxa_microphone_permission() } {
        1 => Ok(()),
        0 => {
            let (tx, rx) = oneshot::channel::<bool>();
            extern "C" fn complete(granted: bool, context: *mut c_void) {
                let sender = unsafe { Box::from_raw(context.cast::<oneshot::Sender<bool>>()) };
                let _ = sender.send(granted);
            }
            unsafe { voxa_request_microphone(complete, Box::into_raw(Box::new(tx)).cast()) };
            if rx.await.unwrap_or(false) {
                Ok(())
            } else {
                bail!("Chưa có quyền Microphone. Mở System Settings → Privacy & Security → Microphone và bật Voxa.")
            }
        }
        _ => bail!("Chưa có quyền Microphone. Mở System Settings → Privacy & Security → Microphone và bật Voxa."),
    }
}

pub struct Capture {
    pub samples: ArrayQueue<f32>,
    pub ready: AtomicBool,
    pub pressed: AtomicBool,
    pub frames: AtomicU64,
    peak: AtomicU32,
    gated: bool,
}

impl Capture {
    pub fn new(gated: bool) -> Self {
        Self {
            samples: ArrayQueue::new(16_384),
            ready: AtomicBool::new(false),
            pressed: AtomicBool::new(false),
            frames: AtomicU64::new(0),
            peak: AtomicU32::new(0),
            gated,
        }
    }

    fn write<T: Sample>(&self, samples: &[T], channels: usize)
    where
        f32: FromSample<T>,
    {
        if self.gated && !self.pressed.load(Relaxed) {
            return;
        }
        let capture = self.ready.load(Relaxed);
        let mut peak = 0.0_f32;
        for frame in samples.chunks_exact(channels) {
            let mono = frame
                .iter()
                .map(|&sample| sample.to_sample::<f32>())
                .sum::<f32>()
                / channels as f32;
            peak = peak.max(mono.abs());
            if capture {
                self.samples.force_push(mono);
            }
        }
        self.peak.fetch_max(peak.to_bits(), Relaxed);
        self.frames
            .fetch_add((samples.len() / channels) as u64, Relaxed);
    }

    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Relaxed))
    }

    pub fn clear(&self) {
        while self.samples.pop().is_some() {}
    }
}

struct Tap {
    handle: *mut c_void,
    capture: Arc<Capture>,
}

impl Tap {
    fn open(source: Option<&str>, capture: Arc<Capture>) -> Result<(Self, u32)> {
        let source = source.map(CString::new).transpose()?;
        extern "C" fn receive(samples: *const f32, count: usize, context: *mut c_void) {
            if count == 0 {
                return;
            }
            let capture = unsafe { &*context.cast::<Capture>() };
            let samples = unsafe { std::slice::from_raw_parts(samples, count) };
            capture.write(samples, 1);
        }
        let mut rate = 0.0;
        let mut status = 0;
        let handle = unsafe {
            voxa_start_tap(
                source
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                receive,
                Arc::as_ptr(&capture).cast_mut().cast(),
                &mut rate,
                &mut status,
            )
        };
        if handle.is_null() {
            match status {
                -1 => bail!("Nguồn đã chọn chưa phát tiếng hoặc đã đóng. Hãy phát tiếng rồi làm mới nguồn."),
                -2 => bail!("Core Audio trả về định dạng âm thanh hệ thống chưa hỗ trợ."),
                -3 => bail!("Core Audio chưa nhận diện tiến trình Voxa để loại trừ tiếng dịch. Hãy thử lại."),
                _ => bail!("Không mở được âm thanh hệ thống (Core Audio {status}). Kiểm tra quyền System Audio Recording trong Privacy & Security."),
            }
        }
        Ok((Self { handle, capture }, rate.round() as u32))
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.capture.ready.store(false, Relaxed);
        unsafe { voxa_stop_tap(self.handle) };
    }
}

pub enum Input {
    System(Option<String>),
    Microphone(Option<String>),
}

pub struct Setup {
    pub input: Input,
    pub output: Option<String>,
    pub route_to_meeting: bool,
}

pub struct AudioIo {
    pub rate: u32,
    pub playback: Arc<Playback>,
    pub failed: Arc<AtomicBool>,
    pub output_name: String,
    _release: mpsc::Sender<()>,
}

impl AudioIo {
    pub async fn open(setup: Setup, capture: Arc<Capture>) -> Result<Self> {
        let (tx, rx) = oneshot::channel();
        let (release, wait_for_release) = mpsc::channel();
        let failed = Arc::new(AtomicBool::new(false));
        let thread_failed = failed.clone();
        std::thread::Builder::new()
            .name("voxa-audio".into())
            .spawn(move || {
                let opened = open_devices(setup, capture, thread_failed);
                match opened {
                    Ok(audio) => {
                        if tx
                            .send(Ok((
                                audio.rate,
                                audio.playback.clone(),
                                audio.output_name.clone(),
                            )))
                            .is_ok()
                        {
                            let _ = wait_for_release.recv();
                        }
                        drop(audio);
                    }
                    Err(error) => {
                        let _ = tx.send(Err(error));
                    }
                }
            })?;
        let (rate, playback, output_name) =
            rx.await.context("Không thể khởi tạo luồng Core Audio.")??;
        Ok(Self {
            rate,
            playback,
            failed,
            output_name,
            _release: release,
        })
    }
}

struct Streams {
    rate: u32,
    playback: Arc<Playback>,
    output_name: String,
    _tap: Option<Tap>,
    _microphone: Option<Stream>,
    _output: Stream,
}

fn choose_device(
    devices: impl Iterator<Item = Device>,
    default: Option<Device>,
    name: Option<&str>,
    missing: &str,
) -> Result<Device> {
    match name {
        Some(name) => devices
            .into_iter()
            .find(|device| device.name().is_ok_and(|actual| actual == name)),
        None => default,
    }
    .ok_or_else(|| anyhow!("{missing}"))
}

fn open_playback(
    output: Option<&str>,
    route_to_meeting: bool,
    failed: Arc<AtomicBool>,
) -> Result<(Stream, Arc<Playback>, String)> {
    let host = cpal::default_host();
    let output = choose_device(
        host.output_devices()?,
        host.default_output_device(),
        output,
        if route_to_meeting {
            "Không tìm thấy BlackHole đã chọn."
        } else {
            "Không tìm thấy loa hoặc tai nghe đã chọn."
        },
    )?;
    let output_name = output
        .name()
        .context("Không đọc được tên thiết bị đầu ra.")?;
    validate_output(&output_name, route_to_meeting)?;
    let config = output
        .default_output_config()
        .context("Không mở được cấu hình thiết bị phát.")?;
    let playback = Arc::new(Playback::new(config.sample_rate().0));
    let output_stream = match config.sample_format() {
        SampleFormat::F32 => {
            output_stream::<f32>(&output, &config.config(), playback.clone(), failed.clone())
        }
        SampleFormat::I16 => {
            output_stream::<i16>(&output, &config.config(), playback.clone(), failed.clone())
        }
        SampleFormat::U16 => {
            output_stream::<u16>(&output, &config.config(), playback.clone(), failed.clone())
        }
        format => bail!("Thiết bị phát sử dụng định dạng chưa hỗ trợ ({format})."),
    }
    .context("Không mở được thiết bị phát đã chọn.")?;
    output_stream
        .play()
        .context("Không thể phát âm trên thiết bị đã chọn.")?;

    Ok((output_stream, playback, output_name))
}

fn validate_output(output_name: &str, route_to_meeting: bool) -> Result<()> {
    if route_to_meeting && !is_blackhole(output_name) {
        bail!("Đầu ra cuộc họp phải là thiết bị BlackHole.");
    }
    if !route_to_meeting && is_blackhole(output_name) {
        bail!(
            "Chọn loa hoặc tai nghe vật lý cho chiều nghe để tránh gửi tiếng cuộc họp ngược lại."
        );
    }
    Ok(())
}

pub fn test_blackhole(name: &str) -> Result<()> {
    let failed = Arc::new(AtomicBool::new(false));
    let (_stream, playback, _) = open_playback(Some(name), true, failed.clone())?;
    let mut bytes = Vec::with_capacity(12_000);
    for index in 0..6_000 {
        let angle = index as f32 * std::f32::consts::TAU * 520.0 / 24_000.0;
        let fade = (index.min(5_999 - index) as f32 / 240.0).min(1.0);
        let sample = (angle.sin() * fade * 4_000.0) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    playback.enqueue(&bytes, &mut playback.resampler());
    std::thread::sleep(std::time::Duration::from_millis(400));
    if failed.load(Relaxed) || !playback.is_empty() {
        bail!("BlackHole chưa phát được âm thử. Kiểm tra thiết bị và thử lại.");
    }
    Ok(())
}

fn open_devices(setup: Setup, capture: Arc<Capture>, failed: Arc<AtomicBool>) -> Result<Streams> {
    let (output_stream, playback, output_name) = open_playback(
        setup.output.as_deref(),
        setup.route_to_meeting,
        failed.clone(),
    )?;
    let host = cpal::default_host();
    let (tap, mic, rate) = match setup.input {
        Input::System(source) => {
            let (tap, rate) = Tap::open(source.as_deref(), capture)?;
            (Some(tap), None, rate)
        }
        Input::Microphone(name) => {
            let mic = choose_device(
                host.input_devices()?,
                host.default_input_device(),
                name.as_deref(),
                "Không tìm thấy micro đã chọn.",
            )?;
            if is_blackhole(&mic.name()?) {
                bail!("Chọn micro vật lý trong Voxa. Chỉ chọn BlackHole làm micro trong Meet hoặc Zoom.");
            }
            let config = mic
                .default_input_config()
                .context("Không đọc được cấu hình micro.")?;
            let stream = match config.sample_format() {
                SampleFormat::F32 => input_stream::<f32>(&mic, &config.config(), capture, failed),
                SampleFormat::I16 => input_stream::<i16>(&mic, &config.config(), capture, failed),
                SampleFormat::U16 => input_stream::<u16>(&mic, &config.config(), capture, failed),
                format => bail!("Micro sử dụng định dạng chưa hỗ trợ ({format})."),
            }
            .context("Không mở được micro đã chọn.")?;
            stream.play().context("Không thể bắt đầu thu qua micro.")?;
            (None, Some(stream), config.sample_rate().0)
        }
    };
    Ok(Streams {
        rate,
        playback,
        output_name,
        _tap: tap,
        _microphone: mic,
        _output: output_stream,
    })
}

fn input_stream<T>(
    device: &Device,
    config: &StreamConfig,
    capture: Arc<Capture>,
    failed: Arc<AtomicBool>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = usize::from(config.channels.max(1));
    device.build_input_stream(
        config,
        move |samples: &[T], _| capture.write(samples, channels),
        move |_| failed.store(true, Relaxed),
        None,
    )
}

fn output_stream<T>(
    device: &Device,
    config: &StreamConfig,
    playback: Arc<Playback>,
    failed: Arc<AtomicBool>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    device.build_output_stream(
        config,
        move |samples: &mut [T], _| {
            for frame in samples.chunks_exact_mut(channels) {
                frame.fill(T::from_sample(playback.pop()));
            }
        },
        move |_| failed.store(true, Relaxed),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_output_does_not_fall_back_to_speakers_or_accept_listening_audio() {
        assert!(validate_output("BlackHole 2ch", true).is_ok());
        assert!(validate_output("MacBook Pro Speakers", true).is_err());
        assert!(validate_output("BlackHole 2ch", false).is_err());
        assert!(validate_output("AirPods", false).is_ok());
    }
}
