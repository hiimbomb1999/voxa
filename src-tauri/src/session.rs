use crate::{
    audio::{self, AudioIo, Capture, Input, Setup},
    history::{self, Caption, History, Session},
    meeting::{MicSensitivity, PushToTalk, SpeechGate},
    provider, soniox, vieneu,
};
use anyhow::{anyhow, bail, Result};
use futures_util::SinkExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
        Arc, Mutex,
    },
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};
use tokio::{
    sync::mpsc,
    time::{interval, sleep, timeout, Instant, MissedTickBehavior},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;
use voxa::{
    pcm::{Encoder, Playback, PlaybackResampler},
    protocol::{self, Event, Mode},
};

static CAPTION_ID: AtomicU64 = AtomicU64::new(1);
const MAX_MEETING_PCM_BYTES: usize = 48_000 * 120;
const MAX_LIVE_PCM_BYTES: usize = 48 * 2_250;
const LIVE_PCM_CATCH_UP_BYTES: usize = 48 * 1_400;
const LIVE_VOICE_QUEUE_WAIT: Duration = Duration::from_millis(1_800);
const LIVE_VOICE_START_LIMIT: Duration = Duration::from_secs(4);

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lane {
    Listen,
    Talk,
    Assistant,
}

impl Lane {
    pub fn label(self) -> &'static str {
        match self {
            Self::Listen => "listen",
            Self::Talk => "talk",
            Self::Assistant => "assistant",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Listen => 0,
            Self::Talk => 1,
            Self::Assistant => 2,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SpeechProvider {
    #[default]
    Gemini,
    Soniox,
}

impl SpeechProvider {
    fn label(self) -> &'static str {
        match self {
            Self::Gemini => "Gemini",
            Self::Soniox => "Soniox",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub lane: Lane,
    #[serde(default)]
    pub provider: SpeechProvider,
    pub source: Option<String>,
    pub microphone: Option<String>,
    pub output: Option<String>,
    pub language: String,
    pub sound: bool,
    pub voice: Option<String>,
    pub save_history: bool,
    #[serde(default)]
    pub mic_sensitivity: MicSensitivity,
    #[serde(default = "default_true")]
    pub quiet_listen_during_talk: bool,
    #[serde(default)]
    pub duck_original: bool,
}

fn default_true() -> bool {
    true
}

struct Worker {
    lane: Lane,
    cancelled: CancellationToken,
    capture: Arc<Capture>,
    sound: Arc<AtomicBool>,
    tts_unavailable: AtomicBool,
    meeting_turn: Arc<PushToTalk>,
    quiet_listen_during_talk: Arc<AtomicBool>,
    mic_sensitivity: MicSensitivity,
    session: Option<Session>,
    started_at: i64,
}

#[derive(Clone, Serialize)]
struct Status<'a> {
    lane: &'a str,
    status: &'a str,
    detail: &'a str,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Signal {
    lane: &'static str,
    peak: f32,
    output_peak: f32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MeetingLatency {
    engine: &'static str,
    total_ms: u64,
    text_wait_ms: Option<u64>,
    voice_and_output_ms: Option<u64>,
}

impl MeetingLatency {
    fn new(
        service: SpeechProvider,
        local: bool,
        release: Instant,
        first_audio: Instant,
        queued: Option<Instant>,
    ) -> Self {
        let total = first_audio.saturating_duration_since(release).as_millis() as u64;
        let text_wait = queued
            .filter(|_| local || service == SpeechProvider::Soniox)
            .map(|time| (time.saturating_duration_since(release).as_millis() as u64).min(total));
        Self {
            engine: if local {
                "vieneu"
            } else if service == SpeechProvider::Soniox {
                "soniox"
            } else {
                "gemini"
            },
            total_ms: total,
            text_wait_ms: text_wait,
            voice_and_output_ms: text_wait.map(|wait| total - wait),
        }
    }
}

#[derive(Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct ListenLatency {
    caption_ms: Option<u64>,
    audio_ms: Option<u64>,
}

#[derive(Default)]
struct ListenTiming {
    first_input: Option<Instant>,
    latency: ListenLatency,
}

impl ListenTiming {
    fn record_input(&mut self, bytes: &[u8]) {
        if self.first_input.is_none()
            && bytes
                .as_chunks::<2>()
                .0
                .iter()
                .filter(|&&sample| i16::from_le_bytes(sample).unsigned_abs() > 260)
                .take(80)
                .count()
                == 80
        {
            self.first_input = Some(Instant::now());
        }
    }

    fn record_caption(&mut self, app: &AppHandle) {
        if let (Some(first), None) = (self.first_input, self.latency.caption_ms) {
            self.latency.caption_ms = Some(first.elapsed().as_millis() as u64);
            let _ = app.emit("voxa:listen-latency", self.latency);
        }
    }

    fn record_audio(&mut self, app: &AppHandle, playback: &Playback, now: Instant) {
        if let (Some(first), None, Some(output)) = (
            self.first_input,
            self.latency.audio_ms,
            playback.first_output(),
        ) {
            let output = Instant::from_std(output);
            if output >= first && output <= now {
                self.latency.audio_ms = Some((output - first).as_millis() as u64);
                let _ = app.emit("voxa:listen-latency", self.latency);
            }
        }
    }
}

pub struct State {
    pub history: Arc<History>,
    active: Mutex<[Option<Arc<Worker>>; 3]>,
    meeting_turn: Arc<PushToTalk>,
    quiet_listen: Arc<AtomicBool>,
    audio_test: Arc<AtomicBool>,
}

impl State {
    pub fn new(history: History) -> Self {
        Self {
            history: Arc::new(history),
            active: Mutex::new([None, None, None]),
            meeting_turn: Arc::new(PushToTalk::default()),
            quiet_listen: Arc::new(AtomicBool::new(true)),
            audio_test: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn start(&self, app: AppHandle, mut options: Options) -> Result<()> {
        let lane = options.lane;
        if lane == Lane::Assistant {
            options.provider = SpeechProvider::Gemini;
        }
        let soniox_enabled = options.provider == SpeechProvider::Soniox;
        if soniox_enabled && lane == Lane::Listen && !soniox::supports_language(&options.language) {
            bail!("Chọn một ngôn ngữ đích được Soniox hỗ trợ.");
        }
        if lane == Lane::Listen && !valid_language(&options.language) {
            bail!("Chọn một ngôn ngữ đích được Gemini hỗ trợ.");
        }
        if let Some(voice) = &options.voice {
            if let Some(name) = voice.strip_prefix("vieneu:") {
                if lane == Lane::Assistant
                    || (lane == Lane::Listen && !matches!(options.language.as_str(), "vi" | "en"))
                {
                    bail!("VieNeu-TTS hỗ trợ giọng đọc tiếng Việt và tiếng Anh trong Voxa.");
                }
                vieneu::validate(&app, name)?;
            } else if soniox_enabled
                || lane != Lane::Listen
                || !matches!(
                    voice.as_str(),
                    "Kore" | "Puck" | "Charon" | "Aoede" | "Zephyr"
                )
            {
                bail!("Giọng đọc chưa được hỗ trợ.");
            }
        }
        if lane == Lane::Talk && options.output.as_ref().is_none_or(|name| name.is_empty()) {
            bail!("Chọn thiết bị BlackHole để gửi tiếng Anh vào cuộc họp.");
        }
        let key = match options.provider {
            SpeechProvider::Gemini => provider::load_key()?,
            SpeechProvider::Soniox => soniox::load_key()?,
        }
        .ok_or_else(|| {
            anyhow!(
                "Nhập {} API key trong phần Cài đặt.",
                options.provider.label()
            )
        })?;
        let mut slots = self.active.lock().unwrap();
        if self.audio_test.load(Relaxed) {
            bail!("Chờ phép thử âm thanh hoàn tất trước khi bắt đầu dịch.");
        }
        if slots[lane.index()].is_some() {
            bail!("Luồng này đang chạy hoặc đang dừng.");
        }
        if matches!(lane, Lane::Assistant) && slots[Lane::Talk.index()].is_some()
            || matches!(lane, Lane::Talk) && slots[Lane::Assistant.index()].is_some()
        {
            bail!("Dừng chế độ micro kia trước khi đổi chế độ.");
        }

        let session = if options.save_history {
            let shared = (lane != Lane::Assistant)
                .then(|| {
                    slots[..2]
                        .iter()
                        .flatten()
                        .find_map(|worker| worker.session)
                })
                .flatten();
            Some(match shared {
                Some(session) => session,
                None => self.history.create(history::lane_label(lane.label()))?,
            })
        } else {
            None
        };
        if lane == Lane::Listen {
            self.set_quiet_listen(options.quiet_listen_during_talk);
        }
        let worker = Arc::new(Worker {
            lane,
            cancelled: CancellationToken::new(),
            capture: Arc::new(Capture::new(lane != Lane::Listen)),
            sound: Arc::new(AtomicBool::new(options.sound || lane != Lane::Listen)),
            tts_unavailable: AtomicBool::new(false),
            meeting_turn: self.meeting_turn.clone(),
            quiet_listen_during_talk: self.quiet_listen.clone(),
            mic_sensitivity: options.mic_sensitivity,
            session,
            started_at: session.map_or_else(history::timestamp, |value| value.started_at),
        });
        slots[lane.index()] = Some(worker.clone());
        drop(slots);
        let history = self.history.clone();

        tauri::async_runtime::spawn(async move {
            let result = run(&app, &worker, &history, options, &key).await;
            worker.capture.ready.store(false, Relaxed);
            worker.capture.pressed.store(false, Relaxed);
            worker.capture.clear();
            if lane == Lane::Talk {
                worker.meeting_turn.reset();
            }
            app.state::<State>().active.lock().unwrap()[lane.index()] = None;
            if let Err(error) = result {
                status(&app, lane, "error", &error.to_string());
            } else {
                status(&app, lane, "stopped", "");
            }
        });
        Ok(())
    }

    pub fn begin_audio_test(&self) -> Result<AudioTestGuard> {
        let slots = self.active.lock().unwrap();
        if slots.iter().any(Option::is_some) || self.audio_test.swap(true, Relaxed) {
            bail!("Dừng phiên đang chạy hoặc chờ phép thử âm thanh hiện tại hoàn tất.");
        }
        Ok(AudioTestGuard(self.audio_test.clone()))
    }

    pub fn stop(&self, lane: Lane) {
        if let Some(worker) = &self.active.lock().unwrap()[lane.index()] {
            worker.capture.ready.store(false, Relaxed);
            worker.capture.pressed.store(false, Relaxed);
            worker.cancelled.cancel();
        }
    }

    pub fn set_sound(&self, enabled: bool) {
        if let Some(worker) = &self.active.lock().unwrap()[Lane::Listen.index()] {
            worker.sound.store(enabled, Relaxed);
        }
    }

    pub fn set_quiet_listen(&self, enabled: bool) {
        self.quiet_listen.store(enabled, Relaxed);
    }

    pub fn press(&self, lane: Lane, pressed: bool) -> bool {
        if lane == Lane::Listen {
            return false;
        }
        if let Some(worker) = &self.active.lock().unwrap()[lane.index()] {
            if pressed && !worker.capture.ready.load(Relaxed) {
                return false;
            }
            if lane == Lane::Talk {
                if pressed && !worker.meeting_turn.press() {
                    return false;
                }
                worker.capture.pressed.store(pressed, Relaxed);
                if !pressed {
                    return worker.meeting_turn.release();
                }
            } else {
                worker.capture.pressed.store(pressed, Relaxed);
            }
            return true;
        }
        false
    }

    pub fn delete_saved(&self, id: i64, started_at: i64) -> Result<()> {
        let active = self.active.lock().unwrap();
        if active
            .iter()
            .flatten()
            .any(|worker| worker.session.is_some_and(|session| session.id == id))
        {
            bail!("Hãy dừng phiên đang ghi transcript trước khi xóa.");
        }
        self.history.delete(id, started_at)
    }
}

pub struct AudioTestGuard(Arc<AtomicBool>);

impl Drop for AudioTestGuard {
    fn drop(&mut self) {
        self.0.store(false, Relaxed);
    }
}

fn valid_language(language: &str) -> bool {
    // The service publishes BCP-47 targets. The server rejects unsupported targets.
    (2..=16).contains(&language.len())
        && language.as_bytes()[0].is_ascii_alphabetic()
        && language
            .bytes()
            .all(|byte| byte.is_ascii_alphabetic() || byte == b'-')
}

fn status(app: &AppHandle, lane: Lane, name: &str, detail: &str) {
    let _ = app.emit(
        "voxa:status",
        Status {
            lane: lane.label(),
            status: name,
            detail,
        },
    );
}

async fn run(
    app: &AppHandle,
    worker: &Arc<Worker>,
    history: &History,
    options: Options,
    key: &str,
) -> Result<()> {
    status(app, worker.lane, "opening", "Đang mở thiết bị âm thanh…");
    let input = if worker.lane == Lane::Listen {
        Input::System(options.source)
    } else {
        tokio::select! {
            _ = worker.cancelled.cancelled() => return Ok(()),
            permission = audio::microphone_permission() => permission?,
        }
        Input::Microphone(options.microphone)
    };
    let setup = Setup {
        input,
        output: options.output,
        route_to_meeting: worker.lane == Lane::Talk,
        duck_original: worker.lane == Lane::Listen && options.duck_original,
    };
    let io = tokio::select! {
        _ = worker.cancelled.cancelled() => return Ok(()),
        opened = AudioIo::open(setup, worker.capture.clone()) => opened?,
    };
    let translation = worker.lane != Lane::Assistant;
    let mode = if translation {
        Mode::Translate
    } else {
        Mode::Assistant
    };
    let target = if worker.lane == Lane::Talk {
        "en"
    } else {
        &options.language
    };
    let mut captions = Captions::new(app, worker, history);
    let mut attempts = 0;
    let mut retried_policy_close = false;
    let service = options.provider;
    let provider_name = service.label();
    loop {
        if io.failed.load(Relaxed) {
            bail!("Thiết bị âm thanh đã đổi hoặc bị ngắt. Chọn lại thiết bị, rồi bắt đầu lại.");
        }
        let voice = match worker.active_voice(options.voice.as_deref()) {
            Some(voice) => {
                if let Some(name) = voice.strip_prefix("vieneu:") {
                    status(app, worker.lane, "opening", "Đang nạp giọng VieNeu-TTS…");
                    let engine = tokio::select! {
                        _ = worker.cancelled.cancelled() => break,
                        engine = vieneu::Engine::start(app) => engine?,
                    };
                    Some(Voice::Local(Box::new(engine), name.to_owned()))
                } else {
                    Some(Voice::Gemini(
                        provider::client()?,
                        key.to_owned(),
                        voice.to_owned(),
                    ))
                }
            }
            None if service == SpeechProvider::Soniox && !worker.tts_unavailable.load(Relaxed) => {
                Some(Voice::Soniox(Box::new(soniox::Voice::new(
                    key.to_owned(),
                    target.to_owned(),
                ))))
            }
            None => None,
        };
        status(
            app,
            worker.lane,
            if attempts == 0 {
                "connecting"
            } else {
                "reconnecting"
            },
            &format!("Đang kết nối {provider_name}…"),
        );
        let mut acknowledged = false;
        let result = match tokio::select! {
            _ = worker.cancelled.cancelled() => break,
            result = LiveSocket::open(service, key, mode, target, worker.lane == Lane::Talk) => result,
        } {
            Ok(mut socket) => {
                acknowledged = true;
                worker.capture.clear();
                worker.capture.ready.store(true, Relaxed);
                status(app, worker.lane, "ready", &io.output_name);
                let started = Instant::now();
                let result = stream(&mut socket, worker, &io, &mut captions, voice).await;
                if started.elapsed() >= Duration::from_secs(10) || result.is_ok() {
                    attempts = 0;
                }
                result
            }
            Err(error) => Err(error),
        };
        worker.capture.ready.store(false, Relaxed);
        worker.capture.pressed.store(false, Relaxed);
        worker.capture.clear();
        io.playback.clear();
        if worker.lane == Lane::Talk {
            worker.meeting_turn.reset();
        }
        captions.finish_all();
        if worker.cancelled.is_cancelled() {
            break;
        }
        let detail = match result {
            Ok(()) => format!("{provider_name} đang chuyển sang kết nối mới."),
            Err(error) if error.is::<provider::LivePolicyClose>() => {
                if !acknowledged {
                    return Err(live_setup_error(error));
                }
                if retried_policy_close {
                    bail!("Gemini đã đóng phiên đang dịch lần nữa (1008). Kiểm tra kết nối Live trong Cài đặt rồi thử lại.");
                }
                retried_policy_close = true;
                attempts += 1;
                "Gemini đã đóng phiên đang dịch (1008). Đang thử kết nối lại…".to_owned()
            }
            Err(error) => {
                let message = error.to_string();
                if io.failed.load(Relaxed)
                    || ["(401)", "(402)", "(403)", "(429)"]
                        .iter()
                        .any(|code| message.contains(code))
                {
                    return Err(error);
                }
                attempts += 1;
                if attempts > 5 {
                    return Err(error);
                }
                message
            }
        };
        status(app, worker.lane, "reconnecting", &detail);
        tokio::select! {
            _ = worker.cancelled.cancelled() => break,
            _ = sleep(Duration::from_millis(if attempts == 0 { 200 } else { (500 << attempts).min(8000) })) => {}
        }
    }
    captions.finish_all();
    Ok(())
}

fn live_setup_error(error: anyhow::Error) -> anyhow::Error {
    if error.is::<provider::LivePolicyClose>() {
        anyhow!("Gemini chưa chấp nhận cấu hình dịch Live (1008). Kiểm tra quyền dùng model của dự án trong Google AI Studio.")
    } else {
        error
    }
}

pub async fn check_live(key: &str) -> Result<()> {
    handshake(key, Mode::Translate, "en")
        .await
        .map_err(live_setup_error)?;
    Ok(())
}

async fn handshake(key: &str, mode: Mode, target: &str) -> Result<provider::Socket> {
    let mut socket = provider::connect(key).await?;
    confirm_setup(&mut socket, mode, target).await?;
    Ok(socket)
}

async fn confirm_setup(socket: &mut provider::Socket, mode: Mode, target: &str) -> Result<()> {
    provider::send(socket, protocol::setup(mode, target)).await?;
    timeout(Duration::from_secs(9), async {
        loop {
            let message = provider::receive(socket).await?;
            if protocol::parse(&message).contains(&Event::Ready) {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await
    .map_err(|_| anyhow!("Gemini chưa xác nhận cấu hình phiên."))?
}

struct LiveSocket {
    socket: provider::Socket,
    service: SpeechProvider,
}

impl LiveSocket {
    async fn open(
        service: SpeechProvider,
        key: &str,
        mode: Mode,
        target: &str,
        talk: bool,
    ) -> Result<Self> {
        let socket = match service {
            SpeechProvider::Gemini => handshake(key, mode, target).await?,
            SpeechProvider::Soniox => soniox::open_stt(key, target, talk).await?,
        };
        Ok(Self { socket, service })
    }

    async fn audio(&mut self, chunk: Vec<u8>) -> Result<()> {
        match self.service {
            SpeechProvider::Gemini => {
                provider::send(&mut self.socket, protocol::input(&chunk)).await
            }
            SpeechProvider::Soniox => {
                soniox::send(&mut self.socket, Message::Binary(chunk.into())).await
            }
        }
    }

    async fn end_turn(&mut self) -> Result<()> {
        match self.service {
            SpeechProvider::Gemini => provider::send(&mut self.socket, protocol::input_end()).await,
            SpeechProvider::Soniox => soniox::finalize(&mut self.socket).await,
        }
    }

    async fn receive(&mut self) -> Result<Vec<Event>> {
        match self.service {
            SpeechProvider::Gemini => {
                Ok(protocol::parse(&provider::receive(&mut self.socket).await?))
            }
            SpeechProvider::Soniox => Ok(soniox::receive_stt(&mut self.socket).await?.0),
        }
    }

    async fn ping(&mut self) -> Result<()> {
        if self.service == SpeechProvider::Soniox {
            return soniox::keepalive(&mut self.socket).await;
        }
        timeout(
            Duration::from_secs(2),
            self.socket.send(Message::Ping(Vec::new().into())),
        )
        .await
        .map_err(|_| anyhow!("Gemini ngừng phản hồi."))?
        .map_err(|_| anyhow!("Mất kết nối với Gemini."))
    }

    async fn finish_soniox(&mut self, tail: Vec<u8>, captions: &mut Captions<'_>) {
        if self.service != SpeechProvider::Soniox {
            return;
        }
        let _ = timeout(Duration::from_millis(900), async {
            if !tail.is_empty() {
                self.audio(tail).await?;
            }
            soniox::send(&mut self.socket, Message::Text("".into())).await?;
            loop {
                let (events, finished) = soniox::receive_stt(&mut self.socket).await?;
                for event in events {
                    if let Event::Caption {
                        text,
                        is_translation,
                        finished,
                    } = event
                    {
                        captions.push(text, is_translation, finished, None);
                    }
                }
                if finished {
                    return Ok::<_, anyhow::Error>(());
                }
            }
        })
        .await;
    }
}

async fn stream(
    socket: &mut LiveSocket,
    worker: &Arc<Worker>,
    io: &AudioIo,
    captions: &mut Captions<'_>,
    voice: Option<Voice>,
) -> Result<()> {
    let talk = worker.lane == Lane::Talk;
    let listen = worker.lane == Lane::Listen;
    let local_voice = matches!(voice, Some(Voice::Local(..)));
    let service = socket.service;
    let provider_name = service.label();
    let early_phrases = listen && (local_voice || service == SpeechProvider::Soniox);
    let keepalive = Duration::from_secs(if service == SpeechProvider::Soniox {
        8
    } else {
        15
    });
    let mut listen_timing = ListenTiming::default();
    if listen {
        io.playback.track_onset();
    }
    let mut latency_reported = false;
    if talk {
        io.playback.track_onset();
        captions.first_voice_text = None;
    }
    let mut encoder = Encoder::new(io.rate, if talk { 320 } else { 1_600 });
    let mut speech = SpeechGate::new(worker.mic_sensitivity);
    let mut released = None;
    let mut completed = None;
    let mut quiet_since = None;
    let mut pending_pcm = VecDeque::new();
    let mut live_catchup_reported = false;
    let mut timer = interval(Duration::from_millis(10));
    timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut output = io.playback.resampler();
    let mut clock = Instant::now();
    let mut last_response = clock;
    let mut last_ping = clock;
    let mut last_active = None;
    let mut last_translation = None;
    let mut last_frames = worker.capture.frames.load(Relaxed);
    let mut last_input = clock;
    let mut current_status = "";
    let mut turn_sent = false;
    let mut rotate_at = None;

    let voice_worker = voice.map(|voice| {
        let (tx, rx) = VoiceQueue::new(worker.lane, early_phrases);
        let task = tokio::spawn(voice_playback(
            captions.app.clone(),
            worker.clone(),
            voice,
            io.playback.clone(),
            rx,
        ));
        (tx, AbortOnDrop(task))
    });
    let voice_tx = || worker.active_voice(voice_worker.as_ref().map(|(tx, _task)| tx));
    let result = loop {
        tokio::select! {
            _ = worker.cancelled.cancelled() => {
                let tail = if listen { encoder.flush() } else { Vec::new() };
                socket.finish_soniox(tail, captions).await;
                break Ok(());
            }
            _ = timer.tick() => {
                let now = Instant::now();
                if rotate_at.is_some_and(|at| now >= at) {
                    break Ok(());
                }
                if io.failed.load(Relaxed) {
                    break Err(anyhow!("Thiết bị âm thanh đã đổi hoặc bị ngắt. Chọn lại thiết bị, rồi bắt đầu lại."));
                }

                let latest = (io.rate / 5) as usize;
                while worker.capture.samples.len() > latest {
                    worker.capture.samples.pop();
                }
                while let Some(sample) = worker.capture.samples.pop() {
                    if !talk || released.is_none() {
                        encoder.push(sample);
                    }
                }
                while let Some(chunk) = encoder.take_chunk() {
                    if talk {
                        if !worker.meeting_turn.busy() {
                            continue;
                        }
                        for chunk in speech.push(chunk) {
                            socket.audio(chunk).await?;
                            turn_sent = true;
                            completed = None;
                        }
                    } else {
                        if listen {
                            listen_timing.record_input(&chunk);
                        }
                        socket.audio(chunk).await?;
                        turn_sent = true;
                    }
                }
                let pressed = worker.capture.pressed.load(Relaxed);
                let end_turn = if talk {
                    worker.meeting_turn.replying() && released.is_none()
                } else {
                    worker.lane == Lane::Assistant && !pressed
                };
                if end_turn {
                    let tail = encoder.flush();
                    if talk && !turn_sent {
                        speech = SpeechGate::new(worker.mic_sensitivity);
                        worker.meeting_turn.reset();
                        let _ = captions.app.emit("voxa:notice", "Chưa nhận đủ tiếng nói. Giữ phím lâu hơn hoặc chọn độ nhạy cao hơn.");
                    } else {
                        if !tail.is_empty() {
                            socket.audio(tail).await?;
                            turn_sent = true;
                            completed = None;
                        }
                        if turn_sent {
                            if talk && service == SpeechProvider::Soniox {
                                socket.audio(vec![0; 6_400]).await?;
                            }
                            socket.end_turn().await?;
                            turn_sent = false;
                            if talk {
                                released = Some(Instant::now());
                            }
                        }
                    }
                }
                if talk && released.is_some() {
                    if !latency_reported {
                        if let (Some(first_output), Some(release)) =
                            (io.playback.first_output(), worker.meeting_turn.released_at())
                        {
                            if first_output >= release && first_output <= now.into_std() {
                                let measurement = MeetingLatency::new(
                                    service, local_voice, release.into(), first_output.into(), captions.first_voice_text,
                                );
                                let _ = captions.app.emit("voxa:meeting-latency", measurement);
                                latency_reported = true;
                            }
                        }
                    }
                    feed_pending_pcm(&mut pending_pcm, &io.playback, &mut output);
                    let speaking = !pending_pcm.is_empty() || !io.playback.is_empty()
                        || voice_tx().is_some_and(VoiceQueue::pending);
                    if speaking {
                        quiet_since = None;
                    } else {
                        let quiet = quiet_since.get_or_insert(now);
                        let reply_finished = completed.is_some_and(|time: Instant| time.elapsed() >= Duration::from_millis(350));
                        if reply_finished && quiet.elapsed() >= Duration::from_millis(280) {
                            captions.finish_all();
                            speech = SpeechGate::new(worker.mic_sensitivity);
                            released = None;
                            completed = None;
                            quiet_since = None;
                            captions.first_voice_text = None;
                            latency_reported = false;
                            io.playback.track_onset();
                            worker.meeting_turn.reset();
                        } else if quiet.elapsed() >= Duration::from_secs(5) {
                            let _ = captions.app.emit("voxa:notice", format!("{provider_name} chưa xác nhận hết lượt nói. Voxa đang kết nối lại."));
                            break Ok(());
                        }
                    }
                }
                if worker.can_play() {
                    if listen {
                        feed_pending_pcm(&mut pending_pcm, &io.playback, &mut output);
                    }
                } else {
                    if listen {
                        pending_pcm.clear();
                    }
                    if !io.playback.is_empty() {
                        io.playback.clear();
                        output = io.playback.resampler();
                    }
                }
                if now.duration_since(last_response) > Duration::from_secs(45) {
                    break Err(anyhow!("{provider_name} ngừng phản hồi."));
                }
                if now.duration_since(last_ping) > keepalive {
                    socket.ping().await?;
                    last_ping = now;
                }
                if now.duration_since(clock) >= Duration::from_millis(120) {
                    clock = now;
                    let frames = worker.capture.frames.load(Relaxed);
                    if frames != last_frames {
                        last_input = now;
                        last_frames = frames;
                    }
                    let peak = worker.capture.take_peak();
                    if peak > 0.002 {
                        last_active = Some(now);
                    }
                    let active = last_active.is_some_and(|when| now.duration_since(when) < Duration::from_secs(2));
                    let (next, detail) = if worker.lane != Lane::Listen {
                        if pressed && talk && !turn_sent { ("waiting_speech", "Đang chờ bạn nói…") }
                        else if pressed { ("speaking", "Đang thu micro…") }
                        else if talk && worker.meeting_turn.replying() { ("replying", "Đang hoàn tất bản dịch…") }
                        else { ("ready", "Giữ phím để nói") }
                    } else if !worker.can_play() && worker.sound.load(Relaxed) {
                        ("listen_quiet", "Đã tạm tắt tiếng trong lượt nói")
                    } else if now.duration_since(last_input) > Duration::from_secs(3) {
                        ("no_signal", "Mac chưa gửi mẫu âm thanh. Kiểm tra quyền và nguồn đã chọn.")
                    } else if last_translation.is_some_and(|when| now.duration_since(when) < Duration::from_secs(2)) {
                        ("translating", "Đang dịch")
                    } else if active {
                        ("listening", "Đang nhận âm thanh")
                    } else {
                        ("silent", "Đang chờ tiếng từ nguồn đã chọn")
                    };
                    if next != current_status {
                        current_status = next;
                        status(captions.app, worker.lane, next, detail);
                    }
                    let _ = captions.app.emit("voxa:signal", Signal {
                        lane: worker.lane.label(),
                        peak: peak.clamp(0.0, 1.0),
                        output_peak: io.playback.take_played_peak().clamp(0.0, 1.0),
                    });
                    if listen {
                        listen_timing.record_audio(captions.app, &io.playback, now);
                    }
                    if !talk || released.is_some() {
                        captions.finish_idle(voice_tx().filter(|_| worker.wants_voice()));
                    }
                }
            }
            result = socket.receive() => {
                let events = result?;
                last_response = Instant::now();
                for event in events {
                    if talk && !turn_sent && released.is_none()
                        && !matches!(event, Event::Ready | Event::RotateAfter(_))
                    {
                        continue;
                    }
                    match event {
                        Event::Ready => {}
                        Event::RotateAfter(delay) => {
                            let next = Instant::now() + delay;
                            rotate_at = Some(rotate_at.map_or(next, |existing: Instant| existing.min(next)));
                        }
                        Event::Caption { text, is_translation, finished } => {
                            if is_translation && !text.trim().is_empty() {
                                last_translation = Some(Instant::now());
                                if listen {
                                    listen_timing.record_caption(captions.app);
                                }
                            }
                            captions.push(text, is_translation, finished, voice_tx().filter(|_| worker.wants_voice()));
                        }
                        Event::ProvisionalCaptions(parts) => {
                            if listen && !parts[1].trim().is_empty() {
                                last_translation = Some(Instant::now());
                                listen_timing.record_caption(captions.app);
                            }
                            captions.provisional(parts);
                        }
                        Event::Audio(bytes) => {
                            last_translation = Some(Instant::now());
                            if voice_tx().is_none() && worker.wants_voice() {
                                if talk {
                                    if pending_pcm.len() + bytes.len() > MAX_MEETING_PCM_BYTES {
                                        bail!("Bản dịch đang chờ quá dài. Nói từng đoạn ngắn hơn.");
                                    }
                                    pending_pcm.extend(bytes);
                                } else if listen {
                                    if append_live_pcm(&mut pending_pcm, &bytes) && !live_catchup_reported {
                                        live_catchup_reported = true;
                                        let _ = captions.app.emit("voxa:notice", "Voxa đã lược âm dịch quá trễ để bám sát video hơn. Nội dung vẫn có trong phụ đề.");
                                    }
                                    feed_pending_pcm(&mut pending_pcm, &io.playback, &mut output);
                                } else {
                                    io.playback.enqueue(&bytes, &mut output);
                                }
                            }
                        }
                        Event::Interrupted => {
                            if listen {
                                // The audio already received belongs to the preceding part of the source.
                                captions.finish(0, None);
                                captions.finish(1, voice_tx().filter(|_| worker.wants_voice()));
                            } else {
                                pending_pcm.clear();
                                io.playback.clear();
                                output = io.playback.resampler();
                                captions.finish_all();
                            }
                        }
                        Event::GenerationComplete => {
                            completed = Some(Instant::now());
                        }
                        Event::TurnComplete => {
                            completed = Some(Instant::now());
                            captions.finish(0, voice_tx().filter(|_| worker.wants_voice()));
                            captions.finish(1, voice_tx().filter(|_| worker.wants_voice()));
                        }
                    }
                }
            }
        }
    };
    drop(voice_worker);
    result
}

fn append_live_pcm(pending: &mut VecDeque<u8>, incoming: &[u8]) -> bool {
    if pending.len() + incoming.len() <= MAX_LIVE_PCM_BYTES {
        pending.extend(incoming);
        return false;
    }

    // Once behind, reduce the queue well below the limit so every small packet
    // does not have to remove a piece of previously queued speech.
    let tail = &incoming[incoming.len().saturating_sub(LIVE_PCM_CATCH_UP_BYTES)..];
    pending.drain(..(pending.len() + tail.len()).saturating_sub(LIVE_PCM_CATCH_UP_BYTES));
    pending.extend(tail);
    true
}

fn feed_pending_pcm(
    pending: &mut VecDeque<u8>,
    playback: &Playback,
    output: &mut PlaybackResampler,
) {
    // Feed at most 50 ms of 24 kHz PCM at a time so the output ring cannot overwrite
    // unplayed samples when a provider returns a long burst at once.
    let bytes = pending.len().min(2_400) / 2 * 2;
    if bytes > 0 && playback.buffered_millis() < 120 {
        let chunk: Vec<u8> = pending.drain(..bytes).collect();
        playback.enqueue(&chunk, output);
    }
}

struct VoiceJob {
    text: String,
    queued_at: Instant,
    work: Arc<()>,
}

impl VoiceJob {
    fn expired(&self, lane: Lane, now: Instant) -> bool {
        if lane != Lane::Listen {
            return false;
        }
        let age = now.saturating_duration_since(self.queued_at);
        // One reference belongs to the sender and one to the current job.
        let newer_waiting = Arc::strong_count(&self.work) > 2;
        age >= LIVE_VOICE_START_LIMIT || (newer_waiting && age >= LIVE_VOICE_QUEUE_WAIT)
    }
}

struct VoiceQueue {
    sender: mpsc::Sender<VoiceJob>,
    work: Arc<()>,
    early_phrases: bool,
    overflow_reported: AtomicBool,
}

impl VoiceQueue {
    fn new(lane: Lane, early_phrases: bool) -> (Self, mpsc::Receiver<VoiceJob>) {
        let (sender, receiver) = mpsc::channel(if lane == Lane::Listen { 128 } else { 12 });
        (
            Self {
                sender,
                work: Arc::new(()),
                early_phrases,
                overflow_reported: AtomicBool::new(false),
            },
            receiver,
        )
    }

    fn submit(&self, text: String) -> Option<Instant> {
        let queued = Instant::now();
        self.sender
            .try_send(VoiceJob {
                text,
                queued_at: queued,
                work: self.work.clone(),
            })
            .ok()?;
        Some(queued)
    }

    fn pending(&self) -> bool {
        // Every queued or in-flight voice job owns one reference.
        Arc::strong_count(&self.work) > 1
    }
}

impl Worker {
    fn active_voice<T>(&self, selected: Option<T>) -> Option<T> {
        selected.filter(|_| !self.tts_unavailable.load(Relaxed))
    }

    fn fall_back_from_tts(&self, error: &anyhow::Error) -> bool {
        if self.lane != Lane::Listen || !error.is::<provider::InteractionRateLimit>() {
            return false;
        }
        self.tts_unavailable.store(true, Relaxed);
        true
    }

    fn wants_voice(&self) -> bool {
        self.sound.load(Relaxed)
            && !(self.lane == Lane::Listen
                && self.quiet_listen_during_talk.load(Relaxed)
                && self.meeting_turn.busy())
    }

    fn can_play(&self) -> bool {
        self.wants_voice() && (self.lane != Lane::Talk || self.meeting_turn.replying())
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

enum Voice {
    Gemini(reqwest::Client, String, String),
    Local(Box<vieneu::Engine>, String),
    Soniox(Box<soniox::Voice>),
}

async fn voice_playback(
    app: AppHandle,
    worker: Arc<Worker>,
    mut voice: Voice,
    playback: Arc<Playback>,
    mut queue: mpsc::Receiver<VoiceJob>,
) {
    let lane = worker.lane;
    let mut catchup_reported = false;
    while let Some(job) = queue.recv().await {
        while lane == Lane::Talk && worker.meeting_turn.holding() {
            sleep(Duration::from_millis(15)).await;
        }
        if !worker.can_play() {
            continue;
        }
        let result = if job.expired(lane, Instant::now()) {
            Ok(false)
        } else {
            play_voice(&mut voice, &job, &playback, &worker).await
        };
        if matches!(result, Ok(false)) && !catchup_reported {
            catchup_reported = true;
            let _ = app.emit(
                "voxa:notice",
                "Voxa đã bỏ lượt đọc quá trễ để giảm độ lệch với video. Nội dung vẫn có trong phụ đề.",
            );
        }
        if let Err(error) = result {
            if worker.fall_back_from_tts(&error) {
                let _ = app.emit(
                    "voxa:notice",
                    "Gemini TTS đã đạt giới hạn (429). Chiều nghe đã chuyển sang giọng Trực tiếp cho phiên này.",
                );
                break;
            }
            if matches!(voice, Voice::Soniox(..)) && soniox::voice_access_denied(&error) {
                worker.tts_unavailable.store(true, Relaxed);
                let _ = app.emit(
                    "voxa:notice",
                    format!("Giọng Soniox · {error} Phụ đề vẫn tiếp tục trong phiên này."),
                );
                break;
            }
            let _ = app.emit(
                "voxa:notice",
                format!("{} · Giọng đọc: {error}", history::lane_label(lane.label())),
            );
            if matches!(voice, Voice::Local(..)) {
                app.state::<State>().stop(lane);
                break;
            }
        }
    }
}

async fn await_first_voice<T>(
    job: &VoiceJob,
    lane: Lane,
    synthesis: impl Future<Output = Result<T>>,
) -> Result<Option<T>> {
    if lane != Lane::Listen {
        return synthesis.await.map(Some);
    }

    tokio::pin!(synthesis);
    let mut check = interval(Duration::from_millis(100));
    check.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            result = &mut synthesis => {
                let output = result?;
                return Ok((!job.expired(lane, Instant::now())).then_some(output));
            }
            _ = check.tick() => {
                if job.expired(lane, Instant::now()) {
                    return Ok(None);
                }
            }
        }
    }
}

async fn play_voice(
    voice: &mut Voice,
    job: &VoiceJob,
    playback: &Playback,
    worker: &Worker,
) -> Result<bool> {
    let text = &job.text;
    let mut resampler = playback.resampler();
    match voice {
        Voice::Local(engine, name) => {
            engine.request(text, name).await?;
            let Some(first) = engine.read().await? else {
                return Ok(true);
            };
            let expired = job.expired(worker.lane, Instant::now());
            let mut play =
                !expired && queue_voice_pcm(&first, playback, worker, &mut resampler).await;
            // Finish reading the response even if it became stale, so VieNeu
            // starts the next request at a response boundary.
            while let Some(pcm) = engine.read().await? {
                if play {
                    play = queue_voice_pcm(&pcm, playback, worker, &mut resampler).await;
                }
            }
            return Ok(!expired);
        }
        Voice::Soniox(engine) => {
            let first = await_first_voice(job, worker.lane, async {
                let mut socket = engine.start(text).await?;
                let pcm = soniox::read_voice(&mut socket).await?;
                Ok((socket, pcm))
            })
            .await?;
            let Some((mut socket, first)) = first else {
                return Ok(false);
            };
            if let Some(first) = first {
                let mut play = queue_voice_pcm(&first, playback, worker, &mut resampler).await;
                while let Some(pcm) = soniox::read_voice(&mut socket).await? {
                    if play {
                        play = queue_voice_pcm(&pcm, playback, worker, &mut resampler).await;
                    }
                }
            }
            engine.reuse(socket);
        }
        Voice::Gemini(client, key, name) => {
            let Some(pcm) =
                await_first_voice(job, worker.lane, provider::voice(client, key, text, name))
                    .await?
            else {
                return Ok(false);
            };
            queue_voice_pcm(&pcm, playback, worker, &mut resampler).await;
        }
    }
    Ok(true)
}

async fn queue_voice_pcm(
    pcm: &[u8],
    playback: &Playback,
    worker: &Worker,
    resampler: &mut PlaybackResampler,
) -> bool {
    for chunk in pcm.chunks(4_800) {
        while playback.buffered_millis() >= 180 {
            sleep(Duration::from_millis(20)).await;
        }
        if !worker.can_play() {
            return false;
        }
        playback.enqueue(chunk, resampler);
    }
    true
}

fn live_phrase_end(text: &str, allow_terminal_end: bool) -> Option<usize> {
    let mut chars = text.char_indices().peekable();
    let mut last_space = None;
    let mut previous = None;
    let mut count = 0;
    while let Some((index, character)) = chars.next() {
        count += 1;
        let end = index + character.len_utf8();
        let boundary = chars
            .peek()
            .map_or(allow_terminal_end, |(_, next)| next.is_whitespace());
        if boundary
            && (matches!(character, '!' | '?' | '…' | '。' | '！' | '？')
                || character == '.' && previous.is_some_and(|ch: char| !ch.is_ascii_digit()))
        {
            return Some(end);
        }
        if character.is_whitespace() {
            last_space = Some(index);
        }
        if count >= 124 {
            if let Some(space) = last_space.filter(|index| *index > 0) {
                return Some(space);
            }
        }
        previous = Some(character);
    }
    None
}

struct Captions<'a> {
    app: &'a AppHandle,
    worker: &'a Worker,
    history: &'a History,
    pending: [Option<(Caption, Instant)>; 2],
    first_voice_text: Option<Instant>,
    spoken_bytes: usize,
}

impl<'a> Captions<'a> {
    fn new(app: &'a AppHandle, worker: &'a Worker, history: &'a History) -> Self {
        Self {
            app,
            worker,
            history,
            pending: [None, None],
            first_voice_text: None,
            spoken_bytes: 0,
        }
    }

    fn provisional(&mut self, parts: [String; 2]) {
        for (index, text) in parts.into_iter().enumerate() {
            if !text.is_empty() && self.pending[index].is_none() {
                let now = history::timestamp().saturating_sub(self.worker.started_at);
                self.pending[index] = Some((
                    Caption {
                        id: CAPTION_ID.fetch_add(1, Relaxed),
                        lane: self.worker.lane.label(),
                        translation: index == 1,
                        text: String::new(),
                        start_ms: now,
                        end_ms: now,
                        finished: false,
                    },
                    Instant::now(),
                ));
            }
            if let Some((caption, updated)) = &mut self.pending[index] {
                if !text.is_empty() {
                    *updated = Instant::now();
                }
                let mut preview = caption.clone();
                preview.text.push_str(&text);
                let _ = self.app.emit("voxa:caption", preview);
            }
        }
    }

    fn push(
        &mut self,
        text: String,
        translation: bool,
        finished: bool,
        queue: Option<&VoiceQueue>,
    ) {
        let index = usize::from(translation);
        if !text.is_empty() {
            let timestamp = history::timestamp().saturating_sub(self.worker.started_at);
            let (caption, received) = self.pending[index].get_or_insert_with(|| {
                (
                    Caption {
                        id: CAPTION_ID.fetch_add(1, Relaxed),
                        lane: self.worker.lane.label(),
                        translation,
                        text: String::new(),
                        start_ms: timestamp,
                        end_ms: timestamp,
                        finished: false,
                    },
                    Instant::now(),
                )
            });
            *received = Instant::now();
            caption.end_ms = timestamp;
            caption.text.push_str(&text);
            if !finished {
                let _ = self.app.emit("voxa:caption", &*caption);
            }
        }
        if finished {
            self.finish(index, queue);
        } else if translation {
            self.queue_live_phrases(queue, false);
        }
    }

    fn queue_live_phrases(&mut self, queue: Option<&VoiceQueue>, after_pause: bool) {
        let Some((caption, _)) = &self.pending[1] else {
            return;
        };
        let Some(queue) = queue else {
            self.spoken_bytes = caption.text.len();
            return;
        };
        if !queue.early_phrases {
            return;
        }
        let mut spoken = self.spoken_bytes;
        while let Some(end) = live_phrase_end(&caption.text[spoken..], after_pause) {
            let phrase = caption.text[spoken..spoken + end].trim();
            if !phrase.is_empty() && queue.submit(phrase.to_owned()).is_none() {
                break;
            }
            spoken += end;
        }
        self.spoken_bytes = spoken;
    }

    fn finish(&mut self, index: usize, queue: Option<&VoiceQueue>) {
        if let Some((mut caption, _)) = self.pending[index].take() {
            let spoken = if caption.translation {
                std::mem::take(&mut self.spoken_bytes)
            } else {
                0
            };
            if caption.text.trim().is_empty() {
                let _ = self.app.emit("voxa:caption", &caption);
                return;
            }
            caption.finished = true;
            let _ = self.app.emit("voxa:caption", &caption);
            if let Some(session) = self.worker.session {
                if self.history.append(session, &caption).is_err() {
                    let _ = self
                        .app
                        .emit("voxa:notice", "Không lưu được một dòng transcript.");
                }
            }
            if let (true, Some(queue)) = (caption.translation, queue) {
                let text = if queue.early_phrases {
                    caption.text[spoken..].trim()
                } else {
                    caption.text.trim()
                };
                if !text.is_empty() {
                    if let Some(queued) = queue.submit(text.to_owned()) {
                        self.first_voice_text.get_or_insert(queued);
                    } else if self.worker.lane == Lane::Listen
                        && !self.worker.tts_unavailable.load(Relaxed)
                        && !queue.overflow_reported.swap(true, Relaxed)
                    {
                        let _ = self.app.emit(
                            "voxa:notice",
                            "Giọng đọc đang chờ quá nhiều câu. Một số đoạn mới chỉ hiện trong phụ đề.",
                        );
                    }
                }
            }
        }
    }

    fn finish_idle(&mut self, queue: Option<&VoiceQueue>) {
        let after_pause = self.pending[1]
            .as_ref()
            .is_some_and(|(_, updated)| updated.elapsed() >= Duration::from_millis(320));
        self.queue_live_phrases(queue, after_pause);
        for index in 0..2 {
            if self.pending[index]
                .as_ref()
                .is_some_and(|(_, updated)| updated.elapsed() >= Duration::from_millis(1_000))
            {
                self.finish(index, queue);
            }
        }
    }

    fn finish_all(&mut self) {
        self.finish(0, None);
        self.finish(1, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[test]
    fn meeting_latency_compares_engines_and_splits_local_wait() {
        let release = Instant::now();
        let output = release + Duration::from_millis(800);
        let text = release + Duration::from_millis(250);
        let gemini =
            MeetingLatency::new(SpeechProvider::Gemini, false, release, output, Some(text));
        assert_eq!(gemini.engine, "gemini");
        assert_eq!(gemini.total_ms, 800);
        assert_eq!(gemini.text_wait_ms, None);
        let local = MeetingLatency::new(SpeechProvider::Gemini, true, release, output, Some(text));
        assert_eq!(local.engine, "vieneu");
        assert_eq!(local.text_wait_ms, Some(250));
        assert_eq!(local.voice_and_output_ms, Some(550));
        let early = MeetingLatency::new(
            SpeechProvider::Gemini,
            true,
            release,
            output,
            Some(release - Duration::from_millis(1)),
        );
        assert_eq!(early.text_wait_ms, Some(0));
        assert_eq!(early.voice_and_output_ms, Some(800));
    }

    #[test]
    fn setup_policy_close_explains_model_access_without_relabeling_other_errors() {
        let policy = live_setup_error(provider::LivePolicyClose.into()).to_string();
        assert!(policy.contains("chưa chấp nhận cấu hình"));
        assert!(policy.contains("1008"));
        let network = live_setup_error(anyhow!("Mất kết nối."));
        assert_eq!(network.to_string(), "Mất kết nối.");
    }

    #[tokio::test]
    async fn websocket_sends_voice_only_after_setup_acknowledgement() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
            let setup = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let setup: serde_json::Value = serde_json::from_str(&setup).unwrap();
            assert_eq!(
                setup["setup"]["generationConfig"]["translationConfig"]["targetLanguageCode"],
                "en"
            );
            assert!(timeout(Duration::from_millis(40), socket.next())
                .await
                .is_err());
            socket
                .send(Message::Text(r#"{"setupComplete":{}}"#.into()))
                .await
                .unwrap();
            let voice = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let voice: serde_json::Value = serde_json::from_str(&voice).unwrap();
            assert_eq!(voice, protocol::input(&[0, 1, 2, 3]));
            let release = socket.next().await.unwrap().unwrap().into_text().unwrap();
            let release: serde_json::Value = serde_json::from_str(&release).unwrap();
            assert_eq!(release, protocol::input_end());
        });
        let (mut socket, _) = tokio_tungstenite::connect_async(format!("ws://{local}"))
            .await
            .unwrap();
        confirm_setup(&mut socket, Mode::Translate, "en")
            .await
            .unwrap();
        provider::send(&mut socket, protocol::input(&[0, 1, 2, 3]))
            .await
            .unwrap();
        provider::send(&mut socket, protocol::input_end())
            .await
            .unwrap();
        server.await.unwrap();
    }
}

#[cfg(test)]
mod meeting_regression {
    use crate::meeting::{MicSensitivity, PushToTalk, SpeechGate};

    #[test]
    fn release_blocks_the_next_press_until_playback_finishes() {
        let gate = PushToTalk::default();
        assert!(!gate.release());
        assert!(gate.press());
        assert!(gate.holding());
        assert!(!gate.press());
        assert!(gate.release());
        assert!(!gate.holding());
        assert!(gate.busy());
        assert!(!gate.press());
        gate.reset();
        assert!(gate.press());
    }

    #[test]
    fn speech_gate_keeps_the_onset_and_ignores_isolated_noise() {
        let sound = [0x00, 0x04].repeat(320);
        let quiet = [0x00, 0x01].repeat(320);
        let mut gate = SpeechGate::new(MicSensitivity::Balanced);
        assert!(gate.push(quiet.clone()).is_empty());
        assert!(gate.push(sound.clone()).is_empty());
        assert!(gate.push(quiet.clone()).is_empty());
        assert!(gate.push(sound.clone()).is_empty());
        assert_eq!(
            gate.push(sound.clone()),
            [
                quiet.clone(),
                sound.clone(),
                quiet.clone(),
                sound.clone(),
                sound
            ]
        );
        assert_eq!(gate.push(quiet.clone()), [quiet]);
    }

    #[test]
    fn noise_filter_does_not_open_on_low_level_background() {
        let quiet = [0_u8, 0x02].repeat(320);
        let mut sensitive = SpeechGate::new(MicSensitivity::Sensitive);
        let mut filtered = SpeechGate::new(MicSensitivity::Noisy);
        assert!(sensitive.push(quiet.clone()).is_empty());
        assert!(filtered.push(quiet.clone()).is_empty());
        assert_eq!(sensitive.push(quiet.clone()).len(), 2);
        assert!(filtered.push(quiet).is_empty());
    }
}

#[cfg(test)]
mod listen_latency_tests {
    use super::*;

    #[test]
    fn translated_phrase_can_play_before_the_remaining_caption_finishes() {
        let text = "Hôm nay chúng ta sẽ họp. Sau đó sẽ kiểm tra";
        let end = live_phrase_end(text, false).unwrap();
        assert_eq!(&text[..end], "Hôm nay chúng ta sẽ họp.");
        assert_eq!(live_phrase_end(&text[end..], false), None);
    }

    #[test]
    fn translated_phrase_waits_for_fragmented_punctuation_and_preserves_numbers() {
        assert_eq!(live_phrase_end("The value is 3.", false), None);
        assert_eq!(
            live_phrase_end("The value is 3.14 and we continue", false),
            None
        );
        assert_eq!(live_phrase_end("Đã xong!", true), Some("Đã xong!".len()));
    }

    #[test]
    fn long_translation_without_punctuation_breaks_at_a_word_boundary() {
        let text = "một cụm rất dài ".repeat(14);
        let end = live_phrase_end(&text, false).unwrap();
        assert!(text.is_char_boundary(end));
        assert!(text[end..].starts_with(' '));
        assert!((90..=145).contains(&text[..end].chars().count()));
    }
}

#[cfg(test)]
mod voice_fallback_tests {
    use super::*;

    #[test]
    fn a_tts_rate_limit_restores_native_audio_for_the_rest_of_the_listen_session() {
        let worker = Worker {
            lane: Lane::Listen,
            cancelled: CancellationToken::new(),
            capture: Arc::new(Capture::new(false)),
            sound: Arc::new(AtomicBool::new(true)),
            tts_unavailable: AtomicBool::new(false),
            meeting_turn: Arc::new(PushToTalk::default()),
            quiet_listen_during_talk: Arc::new(AtomicBool::new(false)),
            mic_sensitivity: MicSensitivity::default(),
            session: None,
            started_at: 0,
        };
        assert_eq!(worker.active_voice(Some("Kore")), Some("Kore"));
        assert!(!worker.fall_back_from_tts(&anyhow!("Some unrelated error (429)")));
        assert_eq!(worker.active_voice(Some("Kore")), Some("Kore"));

        let (queue, _receiver) = VoiceQueue::new(Lane::Listen, false);
        assert!(worker.active_voice(Some(&queue)).is_some());
        assert!(worker.fall_back_from_tts(&provider::InteractionRateLimit.into()));
        assert!(worker.active_voice(Some(&queue)).is_none());
        assert!(worker.wants_voice());
        assert_eq!(worker.active_voice(Some("Kore")), None);
    }
}

#[cfg(test)]
mod continuous_playback_tests {
    use super::*;

    #[test]
    fn a_burst_of_direct_audio_keeps_its_first_and_last_samples() {
        let playback = Playback::new(24_000);
        let mut output = playback.resampler();
        let mut pending = (1_i16..=28_800).flat_map(i16::to_le_bytes).collect();
        for _ in 0..100 {
            feed_pending_pcm(&mut pending, &playback, &mut output);
        }
        assert!(playback.buffered_millis() <= 170);

        let mut samples = Vec::new();
        while !playback.is_empty() {
            samples.push((playback.pop() * 32_768.0) as i16);
            feed_pending_pcm(&mut pending, &playback, &mut output);
        }
        assert!(pending.is_empty());
        assert_eq!(samples.len(), 28_800);
        assert!(samples.into_iter().eq(1..=28_800));
    }

    #[test]
    fn direct_playback_keeps_a_normal_burst_and_trims_only_stale_audio() {
        let mut pending = VecDeque::new();
        let first = vec![1; 48_000];
        let second = vec![2; 48_000];
        assert!(!append_live_pcm(&mut pending, &first));
        assert!(!append_live_pcm(&mut pending, &second));
        assert_eq!(pending.len(), 96_000);
        assert_eq!(pending.front(), Some(&1));

        let next = vec![3; 96_000];
        assert!(append_live_pcm(&mut pending, &next));
        assert_eq!(pending.len(), LIVE_PCM_CATCH_UP_BYTES);
        assert_eq!(pending.front(), Some(&3));
        assert_eq!(pending.back(), Some(&3));
    }

    #[test]
    fn listening_expires_stale_phrases_without_dropping_a_fresh_burst() {
        let (voice, mut receiver) = VoiceQueue::new(Lane::Listen, true);
        voice.submit("Câu cũ".into()).unwrap();
        voice.submit("Câu mới".into()).unwrap();
        let older = receiver.try_recv().unwrap();
        assert!(!older.expired(Lane::Listen, older.queued_at + Duration::from_millis(500)));
        assert!(older.expired(Lane::Listen, older.queued_at + Duration::from_secs(2)));
        assert!(!older.expired(Lane::Talk, older.queued_at + Duration::from_secs(30)));
        drop(older);

        let newest = receiver.try_recv().unwrap();
        assert!(!newest.expired(Lane::Listen, newest.queued_at + Duration::from_secs(2)));
        assert!(newest.expired(Lane::Listen, newest.queued_at + Duration::from_secs(6)));
    }

    #[tokio::test]
    async fn cloud_voice_wait_stops_when_a_newer_phrase_is_queued() {
        let (voice, mut receiver) = VoiceQueue::new(Lane::Listen, true);
        voice.submit("Câu cũ".into()).unwrap();
        voice.submit("Câu mới".into()).unwrap();
        let mut older = receiver.try_recv().unwrap();
        older.queued_at -= Duration::from_secs(2);
        let stalled = std::future::pending::<Result<()>>();
        let result = timeout(
            Duration::from_millis(250),
            await_first_voice(&older, Lane::Listen, stalled),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.is_none());
        drop(older);

        let newer = receiver.try_recv().unwrap();
        let result = await_first_voice(&newer, Lane::Listen, std::future::ready(Ok("fresh PCM")))
            .await
            .unwrap();
        assert_eq!(result, Some("fresh PCM"));
    }

    #[test]
    fn listening_keeps_consecutive_caption_jobs_in_order_while_speech_is_playing() {
        for early_phrases in [true, false] {
            let (voice, mut receiver) = VoiceQueue::new(Lane::Listen, early_phrases);
            for index in 1..=12 {
                assert!(
                    voice.submit(format!("Câu {index}")).is_some(),
                    "Lost clause {index}"
                );
            }
            for index in 1..=12 {
                assert_eq!(receiver.try_recv().unwrap().text, format!("Câu {index}"));
            }
        }
    }
}
