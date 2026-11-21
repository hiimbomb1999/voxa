#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod audio;
mod credentials;
mod history;
mod meeting;
mod provider;
mod session;
mod soniox;
mod vieneu;

use anyhow::Result;
use serde::Serialize;
use session::{Lane, Options, SpeechProvider, State};
#[cfg(target_os = "macos")]
use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

#[cfg(target_os = "macos")]
extern "C" {
    fn voxa_save_dialog(suggested: *const c_char) -> *mut c_char;
    fn voxa_free_string(value: *mut c_char);
}

fn visible_error<T>(result: Result<T>) -> Result<T, String> {
    result.map_err(|error| error.to_string())
}

#[tauri::command]
fn devices() -> Result<audio::Devices, String> {
    visible_error(audio::devices())
}

#[tauri::command]
fn local_voices(app: AppHandle) -> Result<Vec<(String, String)>, String> {
    visible_error(vieneu::voices(&app))
}

#[tauri::command]
async fn test_voice(
    app: AppHandle,
    state: tauri::State<'_, State>,
    output: Option<String>,
    meeting: bool,
    voice: Option<String>,
    language: String,
) -> Result<(), String> {
    let test = visible_error(state.begin_audio_test())?;
    let audio = if let Some(voice) = voice {
        visible_error(vieneu::validate(&app, &voice))?;
        let sample = if meeting || language == "en" {
            vieneu::SAMPLE_EN
        } else {
            vieneu::SAMPLE_VI
        };
        let mut engine = visible_error(vieneu::Engine::start(&app).await)?;
        visible_error(engine.request(sample, &voice).await)?;
        let mut audio = Vec::new();
        while let Some(frame) = visible_error(engine.read().await)? {
            audio.extend(frame);
        }
        audio
    } else if meeting {
        audio::test_tone()
    } else {
        return Err("Chọn một giọng VieNeu-TTS để nghe thử.".to_owned());
    };
    tauri::async_runtime::spawn_blocking(move || {
        let _test = test;
        visible_error(audio::play_test(output.as_deref(), meeting, &audio))
    })
    .await
    .map_err(|_| "Không thể phát âm thử.".to_owned())?
}

#[tauri::command]
async fn test_microphone(
    app: AppHandle,
    state: tauri::State<'_, State>,
    microphone: Option<String>,
    sensitivity: meeting::MicSensitivity,
) -> Result<audio::MicTest, String> {
    let test = visible_error(state.begin_audio_test())?;
    visible_error(audio::microphone_permission().await)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _test = test;
        visible_error(audio::test_microphone(
            microphone.as_deref(),
            sensitivity,
            |level| {
                let _ = app.emit("voxa:microphone-test", level);
            },
        ))
    })
    .await
    .map_err(|_| "Không thể kiểm tra micro.".to_owned())?
}

#[tauri::command]
async fn test_listen_ducking(
    state: tauri::State<'_, State>,
    source: Option<String>,
    output: Option<String>,
) -> Result<audio::DuckingTest, String> {
    let _test = visible_error(state.begin_audio_test())?;
    visible_error(audio::test_ducking(source, output).await)
}

#[tauri::command]
async fn test_blackhole_loopback(
    state: tauri::State<'_, State>,
    device: String,
    speaker: Option<String>,
) -> Result<audio::LoopbackTest, String> {
    let test = visible_error(state.begin_audio_test())?;
    visible_error(audio::microphone_permission().await)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _test = test;
        visible_error(audio::test_blackhole_loopback(&device, speaker.as_deref()))
    })
    .await
    .map_err(|_| format!("Không thể hoàn tất phép thử {}.", audio::MEETING_DEVICE))?
}

#[tauri::command]
fn key_status() -> Result<bool, String> {
    visible_error(provider::load_key().map(|key| key.is_some()))
}

#[tauri::command]
fn store_key(key: String) -> Result<(), String> {
    visible_error(provider::save_key(&key))
}

#[tauri::command]
async fn forget_key(state: tauri::State<'_, State>) -> Result<(), String> {
    visible_error(state.forget_key(SpeechProvider::Gemini).await)
}

#[tauri::command]
fn start(app: AppHandle, state: tauri::State<State>, options: Options) -> Result<(), String> {
    visible_error(state.start(app, options))
}

#[tauri::command]
fn stop(state: tauri::State<State>, lane: Lane) {
    state.stop(lane);
}

#[tauri::command]
fn press(state: tauri::State<State>, lane: Lane, pressed: bool) -> bool {
    state.press(lane, pressed)
}

#[tauri::command]
fn set_listen_sound(state: tauri::State<State>, enabled: bool) {
    state.set_sound(enabled);
}

#[tauri::command]
fn set_quiet_listen(state: tauri::State<State>, enabled: bool) {
    state.set_quiet_listen(enabled);
}

#[tauri::command]
fn soniox_key_status() -> Result<bool, String> {
    visible_error(soniox::load_key().map(|key| key.is_some()))
}

#[tauri::command]
fn store_soniox_key(key: String) -> Result<(), String> {
    visible_error(soniox::store_key(&key))
}

#[tauri::command]
async fn forget_soniox_key(state: tauri::State<'_, State>) -> Result<(), String> {
    visible_error(state.forget_key(SpeechProvider::Soniox).await)
}

#[tauri::command]
async fn check_soniox() -> Result<(), String> {
    let key = visible_error(soniox::load_key())?
        .ok_or_else(|| "Nhập Soniox API key trước khi kiểm tra.".to_owned())?;
    visible_error(soniox::check_key(&key).await)
}

#[tauri::command]
fn soniox_languages() -> Vec<&'static str> {
    soniox::languages().collect()
}

#[tauri::command]
async fn check_gemini_live() -> Result<(), String> {
    let key = visible_error(provider::load_key())?
        .ok_or_else(|| "Nhập Gemini API key trước khi kiểm tra.".to_owned())?;
    visible_error(session::check_live(&key).await)
}

#[tauri::command]
fn overlay(app: AppHandle, show: bool) -> Result<(), String> {
    visible_error(set_overlay(&app, show))
}

fn set_overlay(app: &AppHandle, show: bool) -> Result<()> {
    if let Some(window) = app.get_webview_window("captions") {
        if show {
            window.show()?
        } else {
            window.hide()?
        }
    } else if show {
        let window =
            WebviewWindowBuilder::new(app, "captions", WebviewUrl::App("captions.html".into()))
                .title("Phụ đề Voxa")
                .inner_size(720.0, 170.0)
                .min_inner_size(360.0, 120.0)
                .focused(false)
                .always_on_top(true)
                .visible_on_all_workspaces(true)
                .build()?;
        if let (Some(monitor), Ok(size)) = (window.current_monitor()?, window.outer_size()) {
            let origin = monitor.position();
            let work = monitor.size();
            let x = origin.x + work.width.saturating_sub(size.width) as i32 / 2;
            let y = origin.y + work.height.saturating_sub(size.height + 100) as i32;
            let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
        }
    }
    Ok(())
}

#[tauri::command]
fn history_list(state: tauri::State<State>, before: Option<i64>) -> Result<history::Page, String> {
    visible_error(state.history.list(before))
}

#[tauri::command]
fn history_entries(state: tauri::State<State>, id: i64) -> Result<Vec<history::Entry>, String> {
    visible_error(state.history.entries(id))
}

#[tauri::command]
fn delete_session(state: tauri::State<State>, id: i64, started_at: i64) -> Result<(), String> {
    visible_error(state.delete_saved(id, started_at))
}

#[derive(Serialize)]
struct Exported {
    saved: bool,
}

#[tauri::command]
async fn export_session(
    app: AppHandle,
    state: tauri::State<'_, State>,
    id: i64,
    format: String,
) -> Result<Exported, String> {
    let entries = visible_error(state.history.entries(id))?;
    if entries.is_empty() {
        return Err("Phiên chưa có transcript.".into());
    }
    let text = visible_error(history::render(&entries, &format))?;
    let filename = format!("voxa-{id}.{format}");
    if let Some(path) = transcript_destination(app, filename).await? {
        std::fs::write(path, text)
            .map_err(|error| format!("Không lưu được transcript. {error}"))?;
        Ok(Exported { saved: true })
    } else {
        Ok(Exported { saved: false })
    }
}

#[cfg(target_os = "macos")]
async fn transcript_destination(
    app: AppHandle,
    filename: String,
) -> Result<Option<PathBuf>, String> {
    let filename = CString::new(filename).map_err(|_| "Tên file không hợp lệ.".to_owned())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let raw = unsafe { voxa_save_dialog(filename.as_ptr()) };
        let path = if raw.is_null() {
            None
        } else {
            use std::os::unix::ffi::OsStrExt;
            let bytes = unsafe { CStr::from_ptr(raw) }.to_bytes();
            let path = PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
            unsafe { voxa_free_string(raw) };
            Some(path)
        };
        let _ = tx.send(path);
    })
    .map_err(|_| "Không mở được hộp thoại lưu file.".to_owned())?;
    rx.await
        .map_err(|_| "Không nhận được đường dẫn xuất file.".to_owned())
}

#[cfg(target_os = "windows")]
async fn transcript_destination(
    app: AppHandle,
    filename: String,
) -> Result<Option<PathBuf>, String> {
    let mut dialog = rfd::AsyncFileDialog::new().set_file_name(filename);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    Ok(dialog.save_file().await.map(|file| file.path().to_owned()))
}

#[tauri::command]
async fn summarize_session(state: tauri::State<'_, State>, id: i64) -> Result<String, String> {
    let entries = visible_error(state.history.entries(id))?;
    if entries.is_empty() {
        return Err("Phiên chưa có transcript.".into());
    }
    let key =
        visible_error(provider::load_key())?.ok_or("Nhập Gemini API key trong phần Cài đặt.")?;
    let transcript = visible_error(history::render(&entries, "txt"))?;
    visible_error(provider::summarize(&key, &transcript).await)
}

fn main() {
    let talk = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Space);
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if shortcut == &talk {
                        if let Some(state) = app.try_state::<State>() {
                            state.press(Lane::Talk, event.state() == ShortcutState::Pressed);
                        }
                    }
                })
                .build(),
        )
        .setup(move |app| {
            let directory = app.path().app_data_dir()?;
            app.manage(State::new(history::History::open(&directory)?));
            app.manage(app.global_shortcut().register(talk).is_ok());
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main"
                && matches!(event, tauri::WindowEvent::CloseRequested { .. })
            {
                window.app_handle().exit(0);
            } else if window.label() == "captions"
                && matches!(event, tauri::WindowEvent::CloseRequested { .. })
            {
                let _ = window.app_handle().emit("voxa:overlay-closed", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            devices,
            local_voices,
            test_voice,
            test_microphone,
            test_blackhole_loopback,
            test_listen_ducking,
            key_status,
            store_key,
            forget_key,
            start,
            stop,
            press,
            set_listen_sound,
            set_quiet_listen,
            check_gemini_live,
            soniox_key_status,
            store_soniox_key,
            forget_soniox_key,
            check_soniox,
            soniox_languages,
            overlay,
            history_list,
            history_entries,
            delete_session,
            export_session,
            summarize_session,
            shortcut_available
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| panic!("Không khởi động được Voxa: {error}"));
}

#[tauri::command]
fn shortcut_available(state: tauri::State<bool>) -> bool {
    *state
}
