use crate::{
    audio::{self, AudioIo, Capture, Input, Setup},
    history::{self, Caption, History, Session},
    provider,
};
use anyhow::{anyhow, bail, Result};
use futures_util::SinkExt;
use serde::{Deserialize, Serialize};
use std::{
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
    pcm::{Encoder, Playback},
    protocol::{self, Event, Mode},
};

static CAPTION_ID: AtomicU64 = AtomicU64::new(1);

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub lane: Lane,
    pub source: Option<String>,
    pub microphone: Option<String>,
    pub output: Option<String>,
    pub language: String,
    pub sound: bool,
    pub voice: Option<String>,
    pub save_history: bool,
}

struct Worker {
    lane: Lane,
    cancelled: CancellationToken,
    capture: Arc<Capture>,
    sound: Arc<AtomicBool>,
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
struct Signal {
    lane: &'static str,
    peak: f32,
}

pub struct State {
    pub history: Arc<History>,
    active: Mutex<[Option<Arc<Worker>>; 3]>,
}

impl State {
    pub fn new(history: History) -> Self {
        Self {
            history: Arc::new(history),
            active: Mutex::new([None, None, None]),
        }
    }

    pub fn start(&self, app: AppHandle, options: Options) -> Result<()> {
        let lane = options.lane;
        if lane == Lane::Listen && !valid_language(&options.language) {
            bail!("Chọn một ngôn ngữ đích được Gemini hỗ trợ.");
        }
        if options.voice.as_ref().is_some_and(|voice| {
            !matches!(
                voice.as_str(),
                "Kore" | "Puck" | "Charon" | "Aoede" | "Zephyr"
            )
        }) {
            bail!("Giọng đọc chưa được hỗ trợ.");
        }
        if lane == Lane::Talk && options.output.as_ref().is_none_or(|name| name.is_empty()) {
            bail!("Chọn thiết bị BlackHole để gửi tiếng Anh vào cuộc họp.");
        }
        let key = provider::load_key()?
            .ok_or_else(|| anyhow!("Nhập Gemini API key trong phần Cài đặt."))?;
        let mut slots = self.active.lock().unwrap();
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
        let worker = Arc::new(Worker {
            lane,
            cancelled: CancellationToken::new(),
            capture: Arc::new(Capture::new(lane != Lane::Listen)),
            sound: Arc::new(AtomicBool::new(options.sound || lane != Lane::Listen)),
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
            app.state::<State>().active.lock().unwrap()[lane.index()] = None;
            if let Err(error) = result {
                status(&app, lane, "error", &error.to_string());
            } else {
                status(&app, lane, "stopped", "");
            }
        });
        Ok(())
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

    pub fn press(&self, lane: Lane, pressed: bool) -> bool {
        if lane == Lane::Listen {
            return false;
        }
        if let Some(worker) = &self.active.lock().unwrap()[lane.index()] {
            if !pressed || worker.capture.ready.load(Relaxed) {
                worker.capture.pressed.store(pressed, Relaxed);
                return true;
            }
        }
        false
    }

    pub fn is_saved_active(&self, id: i64) -> bool {
        self.active
            .lock()
            .unwrap()
            .iter()
            .flatten()
            .any(|worker| worker.session.is_some_and(|session| session.id == id))
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
    worker: &Worker,
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
    loop {
        if io.failed.load(Relaxed) {
            bail!("Thiết bị âm thanh đã đổi hoặc bị ngắt. Chọn lại thiết bị, rồi bắt đầu lại.");
        }
        status(
            app,
            worker.lane,
            if attempts == 0 {
                "connecting"
            } else {
                "reconnecting"
            },
            "Đang kết nối Gemini…",
        );
        let result = match tokio::select! {
            _ = worker.cancelled.cancelled() => break,
            result = handshake(key, mode, target) => result,
        } {
            Ok(mut socket) => {
                worker.capture.clear();
                worker.capture.ready.store(true, Relaxed);
                status(app, worker.lane, "ready", &io.output_name);
                let started = Instant::now();
                let result = stream(
                    &mut socket,
                    worker,
                    &io,
                    &mut captions,
                    key,
                    options.voice.as_deref(),
                )
                .await;
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
        captions.finish_all();
        if worker.cancelled.is_cancelled() {
            break;
        }
        let detail = match result {
            Ok(()) => "Gemini đang chuyển sang kết nối mới.".to_owned(),
            Err(error) => {
                let message = error.to_string();
                if io.failed.load(Relaxed)
                    || ["(401)", "(403)", "(1008)", "(429)"]
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

async fn stream(
    socket: &mut provider::Socket,
    worker: &Worker,
    io: &AudioIo,
    captions: &mut Captions<'_>,
    key: &str,
    voice: Option<&str>,
) -> Result<()> {
    let mut encoder = Encoder::new(io.rate, 1_600);
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
        let (tx, rx) = mpsc::channel(2);
        let task = tokio::spawn(voice_playback(
            captions.app.clone(),
            worker.lane,
            key.to_owned(),
            voice.to_owned(),
            io.playback.clone(),
            worker.sound.clone(),
            rx,
        ));
        (tx, AbortOnDrop(task))
    });
    let voice_tx = voice_worker.as_ref().map(|(tx, _task)| tx);
    let result = loop {
        tokio::select! {
            _ = worker.cancelled.cancelled() => break Ok(()),
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
                    encoder.push(sample);
                }
                while let Some(chunk) = encoder.take_chunk() {
                    provider::send(socket, protocol::input(&chunk)).await?;
                    turn_sent = true;
                }
                let pressed = worker.capture.pressed.load(Relaxed);
                if worker.lane != Lane::Listen && !pressed {
                    let tail = encoder.flush();
                    if !tail.is_empty() {
                        provider::send(socket, protocol::input(&tail)).await?;
                        turn_sent = true;
                    }
                    if turn_sent {
                        turn_sent = false;
                        provider::send(socket, protocol::input_end()).await?;
                    }
                }
                if !worker.sound.load(Relaxed) && !io.playback.is_empty() {
                    io.playback.clear();
                }
                if now.duration_since(last_response) > Duration::from_secs(45) {
                    break Err(anyhow!("Gemini ngừng phản hồi."));
                }
                if now.duration_since(last_ping) > Duration::from_secs(15) {
                    timeout(Duration::from_secs(2), socket.send(Message::Ping(Vec::new().into())))
                        .await.map_err(|_| anyhow!("Gemini ngừng phản hồi."))?
                        .map_err(|_| anyhow!("Mất kết nối với Gemini."))?;
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
                        if pressed { ("speaking", "Đang thu micro…") } else { ("ready", "Giữ phím để nói") }
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
                    let _ = captions.app.emit("voxa:signal", Signal { lane: worker.lane.label(), peak: peak.clamp(0.0, 1.0) });
                    captions.finish_idle(voice_tx.filter(|_| worker.sound.load(Relaxed)));
                }
            }
            result = provider::receive(socket) => {
                let value = result?;
                last_response = Instant::now();
                for event in protocol::parse(&value) {
                    match event {
                        Event::Ready => {}
                        Event::RotateAfter(delay) => {
                            let next = Instant::now() + delay;
                            rotate_at = Some(rotate_at.map_or(next, |existing: Instant| existing.min(next)));
                        }
                        Event::Caption { text, is_translation, finished } => {
                            if is_translation && !text.is_empty() {
                                last_translation = Some(Instant::now());
                            }
                            captions.push(text, is_translation, finished, voice_tx.filter(|_| worker.sound.load(Relaxed)));
                        }
                        Event::Audio(bytes) => {
                            last_translation = Some(Instant::now());
                            if voice_worker.is_none() && worker.sound.load(Relaxed) {
                                io.playback.enqueue(&bytes, &mut output);
                            }
                        }
                        Event::Interrupted => {
                            io.playback.clear();
                            output = io.playback.resampler();
                            captions.finish_all();
                        }
                        Event::TurnComplete => {
                            captions.finish(0, voice_tx.filter(|_| worker.sound.load(Relaxed)));
                            captions.finish(1, voice_tx.filter(|_| worker.sound.load(Relaxed)));
                        }
                    }
                }
            }
        }
    };
    drop(voice_worker);
    result
}

type VoiceQueue = mpsc::Sender<(Instant, String)>;

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn voice_playback(
    app: AppHandle,
    lane: Lane,
    key: String,
    voice: String,
    playback: Arc<Playback>,
    sound: Arc<AtomicBool>,
    mut queue: mpsc::Receiver<(Instant, String)>,
) {
    let Ok(client) = provider::client() else {
        return;
    };
    while let Some((queued, text)) = queue.recv().await {
        if !sound.load(Relaxed) || queued.elapsed() > Duration::from_secs(5) {
            continue;
        }
        match provider::voice(&client, &key, &text, &voice).await {
            Ok(bytes) if queued.elapsed() < Duration::from_secs(15) => {
                let mut resampler = playback.resampler();
                for chunk in bytes.chunks(4_800) {
                    while playback.buffered_millis() >= 180 {
                        sleep(Duration::from_millis(20)).await;
                    }
                    if !sound.load(Relaxed) {
                        break;
                    }
                    playback.enqueue(chunk, &mut resampler);
                }
            }
            Ok(_) => {}
            Err(error) => {
                let _ = app.emit(
                    "voxa:notice",
                    format!("{} · Giọng đọc: {error}", history::lane_label(lane.label())),
                );
            }
        }
    }
}

struct Captions<'a> {
    app: &'a AppHandle,
    worker: &'a Worker,
    history: &'a History,
    pending: [Option<(Caption, Instant)>; 2],
}

impl<'a> Captions<'a> {
    fn new(app: &'a AppHandle, worker: &'a Worker, history: &'a History) -> Self {
        Self {
            app,
            worker,
            history,
            pending: [None, None],
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
        }
    }

    fn finish(&mut self, index: usize, queue: Option<&VoiceQueue>) {
        if let Some((mut caption, _)) = self.pending[index].take() {
            if caption.text.trim().is_empty() {
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
            if caption.translation {
                if let Some(queue) = queue {
                    let _ = queue.try_send((Instant::now(), caption.text));
                }
            }
        }
    }

    fn finish_idle(&mut self, queue: Option<&VoiceQueue>) {
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
