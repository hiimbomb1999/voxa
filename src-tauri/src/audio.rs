use anyhow::{anyhow, bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, FromSample, Sample, SampleFormat, SizedSample, Stream, StreamConfig,
};
use crossbeam_queue::ArrayQueue;
use serde::Serialize;
#[cfg(target_os = "macos")]
use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed},
    mpsc, Arc,
};
use tokio::sync::oneshot;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
use windows::Tap;

use voxa::pcm::{DuckingMixer, Encoder, OriginalAudio, OriginalReader, Playback, AUDIBLE_FLOOR};

#[cfg(target_os = "macos")]
extern "C" {
    fn voxa_uses_blackhole(device: *const c_char) -> bool;
    fn voxa_sources() -> *mut c_char;
    fn voxa_free_string(value: *mut c_char);
    fn voxa_microphone_permission() -> i32;
    fn voxa_request_microphone(callback: extern "C" fn(bool, *mut c_void), context: *mut c_void);
    fn voxa_start_tap(
        source: *const c_char,
        monitor: bool,
        callback: extern "C" fn(*const f32, *const f32, usize, usize, u32, *mut c_void),
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
    platform: &'static str,
    sources: Vec<Source>,
    microphones: Vec<String>,
    speakers: Vec<String>,
    blackhole: Vec<String>,
    blackhole_installed: bool,
    default_microphone: Option<String>,
    default_output: Option<String>,
    default_output_loops: bool,
}

#[cfg(target_os = "macos")]
fn app_sources() -> Result<Vec<Source>> {
    let raw = unsafe { voxa_sources() };
    let mut sources: Vec<Source> = if raw.is_null() {
        Vec::new()
    } else {
        let parsed = serde_json::from_slice(unsafe { CStr::from_ptr(raw) }.to_bytes());
        unsafe { voxa_free_string(raw) };
        parsed.context("Không đọc được danh sách nguồn Core Audio.")?
    };
    sources.sort_by_cached_key(|source| source.name.to_lowercase());
    Ok(sources)
}

#[cfg(target_os = "windows")]
fn app_sources() -> Result<Vec<Source>> {
    Ok(Vec::new())
}

pub const MEETING_DEVICE: &str = if cfg!(target_os = "windows") {
    "VB-CABLE"
} else {
    "BlackHole"
};

pub fn devices() -> Result<Devices> {
    let sources = app_sources()?;
    let host = cpal::default_host();
    let default_microphone = host.default_input_device().and_then(|d| d.name().ok());
    let default_output = host.default_output_device().and_then(|d| d.name().ok());
    let default_output_loops = default_output.as_deref().is_some_and(uses_meeting_cable);
    let mut microphones: Vec<String> = host
        .input_devices()?
        .filter_map(|device| device.name().ok())
        .filter(|name| !uses_meeting_cable(name) && name != "Voxa Private Capture")
        .collect();
    let mut speakers = Vec::new();
    let mut blackhole = Vec::new();
    for name in host
        .output_devices()?
        .filter_map(|device| device.name().ok())
    {
        if is_meeting_output(&name) {
            blackhole.push(name);
        } else if name != "Voxa Private Capture" && !uses_meeting_cable(&name) {
            speakers.push(name);
        }
    }
    for names in [&mut microphones, &mut speakers, &mut blackhole] {
        names.sort_unstable();
        names.dedup();
    }
    let blackhole_installed = if cfg!(target_os = "macos") {
        std::path::Path::new("/Library/Audio/Plug-Ins/HAL/BlackHole2ch.driver").is_dir()
    } else {
        !blackhole.is_empty()
    };
    Ok(Devices {
        platform: std::env::consts::OS,
        sources,
        microphones,
        speakers,
        blackhole,
        default_microphone,
        default_output,
        default_output_loops,
        blackhole_installed,
    })
}

#[cfg(target_os = "macos")]
fn is_meeting_output(name: &str) -> bool {
    name.to_ascii_lowercase().starts_with("blackhole")
}

#[cfg(target_os = "macos")]
fn uses_meeting_cable(name: &str) -> bool {
    is_meeting_output(name)
        || CString::new(name).is_ok_and(|name| unsafe { voxa_uses_blackhole(name.as_ptr()) })
}

#[cfg(target_os = "windows")]
fn vb_cable_endpoint(name: &str, direction: &str) -> bool {
    name.to_ascii_lowercase()
        .strip_prefix(direction)
        .is_some_and(|suffix| suffix.trim_start().starts_with('(') && suffix.contains("vb-audio"))
}

#[cfg(target_os = "windows")]
fn is_meeting_output(name: &str) -> bool {
    vb_cable_endpoint(name, "cable input")
}

#[cfg(target_os = "windows")]
fn uses_meeting_cable(name: &str) -> bool {
    is_meeting_output(name) || vb_cable_endpoint(name, "cable output")
}

#[cfg(target_os = "windows")]
pub async fn microphone_permission() -> Result<()> {
    // Windows desktop microphone access is controlled by the system privacy setting.
    Ok(())
}

#[cfg(target_os = "macos")]
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
        Self::with_capacity(gated, 16_384)
    }

    fn with_capacity(gated: bool, capacity: usize) -> Self {
        Self {
            samples: ArrayQueue::new(capacity),
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
        let frames = samples.chunks_exact(channels).map(|frame| {
            frame
                .iter()
                .map(|&sample| sample.to_sample::<f32>())
                .sum::<f32>()
                / channels as f32
        });
        self.write_mono(frames);
    }

    fn write_mono(&self, frames: impl ExactSizeIterator<Item = f32>) {
        let capture = self.ready.load(Relaxed);
        let count = frames.len();
        let mut peak = 0.0_f32;
        for mono in frames {
            peak = peak.max(mono.abs());
            if capture {
                self.samples.force_push(mono);
            }
        }
        self.peak.fetch_max(peak.to_bits(), Relaxed);
        self.frames.fetch_add(count as u64, Relaxed);
    }

    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Relaxed))
    }

    pub fn clear(&self) {
        while self.samples.pop().is_some() {}
    }
}

#[cfg(target_os = "macos")]
struct TapContext {
    capture: Arc<Capture>,
    original: Option<Arc<OriginalAudio>>,
}

#[cfg(target_os = "macos")]
struct Tap {
    handle: *mut c_void,
    context: Box<TapContext>,
}

#[cfg(target_os = "macos")]
impl Tap {
    fn open(
        source: Option<&str>,
        capture: Arc<Capture>,
        original: Option<Arc<OriginalAudio>>,
    ) -> Result<(Self, u32)> {
        let source = source.map(CString::new).transpose()?;
        let mut context = Box::new(TapContext { capture, original });
        extern "C" fn receive(
            left: *const f32,
            right: *const f32,
            count: usize,
            stride: usize,
            rate: u32,
            context: *mut c_void,
        ) {
            let context = unsafe { &*context.cast::<TapContext>() };
            if let Some(original) = &context.original {
                original.set_rate(rate);
            }
            let mono = (0..count).map(|frame| {
                let index = frame * stride;
                let stereo = unsafe { [*left.add(index), *right.add(index)] };
                if let Some(original) = &context.original {
                    original.push(stereo);
                }
                (stereo[0] + stereo[1]) * 0.5
            });
            context.capture.write_mono(mono);
        }
        let mut rate = 0.0;
        let mut status = 0;
        let handle = unsafe {
            voxa_start_tap(
                source
                    .as_ref()
                    .map_or(std::ptr::null(), |value| value.as_ptr()),
                context.original.is_some(),
                receive,
                std::ptr::from_mut(context.as_mut()).cast(),
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
        Ok((Self { handle, context }, rate.round() as u32))
    }
}

#[cfg(target_os = "macos")]
impl Drop for Tap {
    fn drop(&mut self) {
        self.context.capture.ready.store(false, Relaxed);
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
    pub duck_original: bool,
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
            rx.await.context("Không thể khởi tạo luồng âm thanh.")??;
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
    original: Option<Arc<OriginalAudio>>,
) -> Result<(Stream, Arc<Playback>, String)> {
    let host = cpal::default_host();
    let output = choose_device(
        host.output_devices()?,
        host.default_output_device(),
        output,
        if route_to_meeting {
            "Không tìm thấy cáp âm thanh đã chọn."
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
        SampleFormat::F32 => output_stream::<f32>(
            &output,
            &config.config(),
            playback.clone(),
            failed.clone(),
            original,
        ),
        SampleFormat::I16 => output_stream::<i16>(
            &output,
            &config.config(),
            playback.clone(),
            failed.clone(),
            original,
        ),
        SampleFormat::U16 => output_stream::<u16>(
            &output,
            &config.config(),
            playback.clone(),
            failed.clone(),
            original,
        ),
        format => bail!("Thiết bị phát sử dụng định dạng chưa hỗ trợ ({format})."),
    }
    .context("Không mở được thiết bị phát đã chọn.")?;
    output_stream
        .play()
        .context("Không thể phát âm trên thiết bị đã chọn.")?;

    Ok((output_stream, playback, output_name))
}

fn validate_output(output_name: &str, route_to_meeting: bool) -> Result<()> {
    if route_to_meeting && !is_meeting_output(output_name) {
        bail!("Đầu ra cuộc họp phải là thiết bị {MEETING_DEVICE}.");
    }
    if !route_to_meeting && uses_meeting_cable(output_name) {
        bail!(
            "Chọn loa hoặc tai nghe vật lý cho chiều nghe để tránh gửi tiếng cuộc họp ngược lại."
        );
    }
    Ok(())
}

pub fn test_tone() -> Vec<u8> {
    tone(520.0, 24_000)
}

fn tone(frequency: f32, samples: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples * 2);
    for index in 0..samples {
        let angle = index as f32 * std::f32::consts::TAU * frequency / 24_000.0;
        let fade = (index.min(samples - 1 - index) as f32 / 240.0).min(1.0);
        let sample = (angle.sin() * fade * 4_000.0) as i16;
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

pub fn play_test(name: Option<&str>, meeting: bool, bytes: &[u8]) -> Result<()> {
    let failed = Arc::new(AtomicBool::new(false));
    let (_stream, playback, _) = open_playback(name, meeting, failed.clone(), None)?;
    play_sample(&playback, &failed, bytes)
}

#[derive(Serialize)]
pub struct DuckingTest {
    output: String,
    source_detected: bool,
}

pub async fn test_ducking(source: Option<String>, output: Option<String>) -> Result<DuckingTest> {
    let capture = Arc::new(Capture::new(false));
    let io = AudioIo::open(
        Setup {
            input: Input::System(source),
            output,
            route_to_meeting: false,
            duck_original: true,
        },
        capture.clone(),
    )
    .await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    let source_detected = capture.take_peak() > 0.002;
    let playback = io.playback.clone();
    let failed = io.failed.clone();
    tokio::task::spawn_blocking(move || play_sample(&playback, &failed, &test_tone())).await??;
    tokio::time::sleep(std::time::Duration::from_millis(650)).await;
    Ok(DuckingTest {
        output: io.output_name.clone(),
        source_detected,
    })
}

fn play_sample(playback: &Playback, failed: &AtomicBool, bytes: &[u8]) -> Result<()> {
    let mut resampler = playback.resampler();
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(bytes.len() as u64 / 48 + 1_500);
    for chunk in bytes.chunks(4_800) {
        while playback.buffered_millis() >= 180 {
            check_test_output(failed, deadline)?;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        playback.enqueue(chunk, &mut resampler);
    }
    while !playback.is_empty() {
        check_test_output(failed, deadline)?;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    check_test_output(failed, deadline)?;
    // Allow Core Audio to deliver the final buffer after the queue drains.
    std::thread::sleep(std::time::Duration::from_millis(100));
    Ok(())
}

const CHECK_TONES: [f32; 2] = [520.0, 780.0];
const TONE_WINDOW_BYTES: usize = 2_400;

fn loopback_tone() -> Vec<u8> {
    let mut bytes = Vec::new();
    for frequency in CHECK_TONES {
        bytes.extend(tone(frequency, 9_600));
    }
    bytes
}

fn tone_window(window: &[u8], frequency: f32) -> bool {
    let (mut sine, mut cosine, mut energy) = (0.0, 0.0, 0.0);
    let step = std::f32::consts::TAU * frequency / 24_000.0;
    for (index, &bytes) in window.as_chunks::<2>().0.iter().enumerate() {
        let sample = f32::from(i16::from_le_bytes(bytes)) / 32_768.0;
        let (sin, cos) = (index as f32 * step).sin_cos();
        sine += sample * sin;
        cosine += sample * cos;
        energy += sample * sample;
    }
    let samples = (window.len() / 2) as f32;
    energy > samples * 0.000_01 && 2.0 * (sine * sine + cosine * cosine) > 0.82 * samples * energy
}

fn find_tone(bytes: &[u8], frequency: f32) -> Option<std::ops::Range<usize>> {
    let mut run = None;
    let mut end = 0;
    for window in bytes.as_chunks::<TONE_WINDOW_BYTES>().0 {
        if tone_window(window, frequency) {
            run.get_or_insert(end);
        } else if let Some(start) = run.take() {
            if end - start >= 3 * TONE_WINDOW_BYTES {
                return Some(start..end);
            }
        }
        end += TONE_WINDOW_BYTES;
    }
    run.filter(|start| end - start >= 3 * TONE_WINDOW_BYTES)
        .map(|start| start..end)
}

fn confirmed_loopback(bytes: &[u8]) -> Result<Vec<u8>> {
    let missing = || {
        anyhow!("Không thu lại đủ hai âm thử từ {MEETING_DEVICE}. Kiểm tra thiết bị và quyền micro rồi thử lại.")
    };
    let first = find_tone(bytes, CHECK_TONES[0]).ok_or_else(missing)?;
    let remaining = &bytes[first.end..];
    let second = find_tone(remaining, CHECK_TONES[1]).ok_or_else(missing)?;
    if second.start > 3 * TONE_WINDOW_BYTES {
        return Err(missing());
    }
    // Replay the confirmed parts of the capture, excluding unrelated input.
    let mut captured = bytes[first].to_vec();
    captured.extend_from_slice(&remaining[second]);
    Ok(captured)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopbackTest {
    device: String,
    speaker: String,
    duration_ms: usize,
}

pub fn test_blackhole_loopback(device: &str, speaker: Option<&str>) -> Result<LoopbackTest> {
    if !is_meeting_output(device) {
        bail!("Chọn thiết bị {MEETING_DEVICE} để nghe lại.");
    }
    #[cfg(target_os = "macos")]
    let input = device.to_owned();
    let host = cpal::default_host();
    #[cfg(target_os = "macos")]
    let input = choose_device(
        host.input_devices()?,
        None,
        Some(&input),
        "Không tìm thấy đầu thu của cáp âm thanh.",
    )?;
    #[cfg(target_os = "windows")]
    let input = host
        .input_devices()?
        .find(|input| {
            input
                .name()
                .is_ok_and(|name| vb_cable_endpoint(&name, "cable output"))
        })
        .ok_or_else(|| anyhow!("Không tìm thấy CABLE Output."))?;
    let failed = Arc::new(AtomicBool::new(false));
    // Validate the physical monitor before sending any audio to BlackHole.
    let (_monitor, monitor, speaker) = open_playback(speaker, false, failed.clone(), None)?;
    let (sender, playback, device) = open_playback(Some(device), true, failed.clone(), None)?;
    let capture = Arc::new(Capture::with_capacity(false, 384_000));
    let (receiver, rate) = open_input(&input, capture.clone(), failed.clone())?;
    capture.ready.store(true, Relaxed);
    std::thread::sleep(std::time::Duration::from_millis(80));
    play_sample(&playback, &failed, &loopback_tone())?;
    capture.ready.store(false, Relaxed);
    drop((receiver, sender));

    let mut encoder = Encoder::with_output_rate(rate, 24_000, 1_600);
    while let Some(sample) = capture.samples.pop() {
        encoder.push(sample);
    }
    let audio = confirmed_loopback(&encoder.flush())?;
    play_sample(&monitor, &failed, &audio)?;
    Ok(LoopbackTest {
        device,
        speaker,
        duration_ms: audio.len() / 48,
    })
}

fn check_test_output(failed: &AtomicBool, deadline: std::time::Instant) -> Result<()> {
    if failed.load(Relaxed) || std::time::Instant::now() > deadline {
        bail!("Thiết bị chưa phát được âm thử. Kiểm tra thiết bị và thử lại.");
    }
    Ok(())
}

fn open_devices(setup: Setup, capture: Arc<Capture>, failed: Arc<AtomicBool>) -> Result<Streams> {
    #[cfg(target_os = "windows")]
    if matches!(setup.input, Input::System(_)) && setup.duck_original {
        bail!("Bản thử Windows chưa hỗ trợ giảm tiếng gốc tự động.");
    }
    let original = (setup.duck_original && matches!(setup.input, Input::System(_)))
        .then(|| Arc::new(OriginalAudio::default()));
    let (output_stream, playback, output_name) = open_playback(
        setup.output.as_deref(),
        setup.route_to_meeting,
        failed.clone(),
        original.clone(),
    )?;
    let (tap, mic, rate) = match setup.input {
        Input::System(source) => {
            #[cfg(target_os = "macos")]
            let (tap, rate) = Tap::open(source.as_deref(), capture, original)?;
            #[cfg(target_os = "windows")]
            let (tap, rate) = {
                if source.is_some() {
                    bail!("Bản thử Windows hiện thu âm thanh toàn hệ thống.");
                }
                Tap::open(capture, failed)?
            };
            (Some(tap), None, rate)
        }
        Input::Microphone(name) => {
            let (stream, rate, _) = open_microphone(name.as_deref(), capture, failed)?;
            (None, Some(stream), rate)
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

fn open_microphone(
    name: Option<&str>,
    capture: Arc<Capture>,
    failed: Arc<AtomicBool>,
) -> Result<(Stream, u32, String)> {
    let host = cpal::default_host();
    let mic = choose_device(
        host.input_devices()?,
        host.default_input_device(),
        name,
        "Không tìm thấy micro đã chọn.",
    )?;
    let name = mic.name()?;
    if uses_meeting_cable(&name) {
        bail!("Chọn micro vật lý cho Voxa. Thiết bị này là cáp {MEETING_DEVICE}.");
    }
    let (stream, rate) = open_input(&mic, capture, failed)?;
    Ok((stream, rate, name))
}

fn open_input(
    device: &Device,
    capture: Arc<Capture>,
    failed: Arc<AtomicBool>,
) -> Result<(Stream, u32)> {
    let config = device
        .default_input_config()
        .context("Không đọc được cấu hình đầu vào.")?;
    let stream = match config.sample_format() {
        SampleFormat::F32 => input_stream::<f32>(device, &config.config(), capture, failed),
        SampleFormat::I16 => input_stream::<i16>(device, &config.config(), capture, failed),
        SampleFormat::U16 => input_stream::<u16>(device, &config.config(), capture, failed),
        format => bail!("Đầu vào sử dụng định dạng chưa hỗ trợ ({format})."),
    }
    .context("Không mở được đầu vào. Kiểm tra thiết bị và quyền micro trong cài đặt hệ thống.")?;
    stream.play().context("Không thể bắt đầu thu âm.")?;
    Ok((stream, config.sample_rate().0))
}

#[derive(Serialize)]
pub struct MicTest {
    name: String,
    peak: f32,
}

pub fn test_microphone(name: Option<&str>, on_level: impl Fn(f32)) -> Result<MicTest> {
    let capture = Arc::new(Capture::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let (_stream, _, name) = open_microphone(name, capture.clone(), failed.clone())?;
    let mut peak = 0.0_f32;
    for _ in 0..24 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        if failed.load(Relaxed) {
            bail!("Micro bị ngắt trong lúc kiểm tra.");
        }
        let level = capture.take_peak();
        peak = peak.max(level);
        on_level(level);
    }
    Ok(MicTest { name, peak })
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
    original: Option<Arc<OriginalAudio>>,
) -> Result<Stream, cpal::BuildStreamError>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels.max(1));
    let rate = config.sample_rate.0;
    let mut original =
        original.map(|audio| (audio, OriginalReader::new(rate), DuckingMixer::new(rate)));
    device.build_output_stream(
        config,
        move |samples: &mut [T], info| {
            let callback_at = std::time::Instant::now();
            let mut peak = 0.0_f32;
            let mut waiting = playback.awaiting_onset();
            if let Some((audio, reader, _)) = &mut original {
                reader.start_buffer(audio);
            }
            for (index, frame) in samples.chunks_exact_mut(channels).enumerate() {
                let value = playback.pop();
                peak = peak.max(value.abs());
                if let Some((audio, reader, mixer)) = &mut original {
                    let mixed = mixer.mix(reader.next(audio), value);
                    if channels == 1 {
                        frame[0] = T::from_sample((mixed[0] + mixed[1]) * 0.5);
                    } else {
                        frame[0] = T::from_sample(mixed[0]);
                        frame[1] = T::from_sample(mixed[1]);
                        frame[2..].fill(T::from_sample(0.0));
                    }
                } else {
                    frame.fill(T::from_sample(value));
                }
                if waiting && value.abs() > AUDIBLE_FLOOR {
                    let timestamp = info.timestamp();
                    let device_delay = timestamp
                        .playback
                        .duration_since(&timestamp.callback)
                        .unwrap_or_default();
                    let frame_delay =
                        std::time::Duration::from_secs_f64(index as f64 / f64::from(rate));
                    playback.report_onset(value, callback_at + device_delay + frame_delay);
                    waiting = false;
                }
            }
            playback.report_played_peak(peak);
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
        let cable = if cfg!(target_os = "windows") {
            "CABLE Input (VB-Audio Virtual Cable)"
        } else {
            "BlackHole 2ch"
        };
        assert!(validate_output(cable, true).is_ok());
        assert!(validate_output("Speakers", true).is_err());
        assert!(validate_output(cable, false).is_err());
        assert!(validate_output("AirPods", false).is_ok());
    }
}

#[cfg(test)]
mod loopback_tests {
    use super::*;

    #[test]
    fn finds_two_test_tones_in_returned_audio_with_padding() {
        let mut returned = vec![0; 3_174];
        returned.extend(loopback_tone());
        returned.extend(vec![0; 720]);

        let sample = confirmed_loopback(&returned).unwrap();
        assert!(sample.len() < returned.len());
        assert!(sample.len() >= 24_000);
        assert_eq!(sample.len() % 2, 0);
    }

    #[test]
    fn silence_and_a_single_tone_do_not_confirm_blackhole() {
        assert!(confirmed_loopback(&vec![0; 48_000]).is_err());
        assert!(confirmed_loopback(&test_tone()).is_err());
    }
}
