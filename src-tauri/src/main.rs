mod audio;
mod history;
mod provider;
mod session;

use anyhow::Result;
use serde::Serialize;
use session::{Lane, Options, State};
use std::ffi::{c_char, CStr, CString};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

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
async fn test_blackhole(output: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || visible_error(audio::test_blackhole(&output)))
        .await
        .map_err(|_| "Không thể chạy thử BlackHole.".to_owned())?
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
fn forget_key(state: tauri::State<State>) -> Result<(), String> {
    visible_error(provider::forget_key())?;
    for lane in [Lane::Listen, Lane::Talk, Lane::Assistant] {
        state.stop(lane);
    }
    Ok(())
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
fn history_list(state: tauri::State<State>) -> Result<Vec<history::Saved>, String> {
    visible_error(state.history.list())
}

#[tauri::command]
fn history_entries(state: tauri::State<State>, id: i64) -> Result<Vec<history::Entry>, String> {
    visible_error(state.history.entries(id))
}

#[tauri::command]
fn delete_session(state: tauri::State<State>, id: i64) -> Result<(), String> {
    if state.is_saved_active(id) {
        return Err("Hãy dừng phiên đang chạy trước khi xóa.".to_owned());
    }
    visible_error(state.history.delete(id))
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
    let name = CString::new(format!("voxa-{id}.{format}"))
        .map_err(|_| "Tên file không hợp lệ.".to_owned())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let raw = unsafe { voxa_save_dialog(name.as_ptr()) };
        let path = if raw.is_null() {
            None
        } else {
            use std::os::unix::ffi::OsStrExt;
            let bytes = unsafe { CStr::from_ptr(raw) }.to_bytes();
            let path = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes));
            unsafe { voxa_free_string(raw) };
            Some(path)
        };
        let _ = tx.send(path);
    })
    .map_err(|_| "Không mở được hộp thoại lưu file.".to_owned())?;
    if let Some(path) = rx
        .await
        .map_err(|_| "Không nhận được đường dẫn xuất file.".to_owned())?
    {
        std::fs::write(path, text)
            .map_err(|error| format!("Không lưu được transcript. {error}"))?;
        Ok(Exported { saved: true })
    } else {
        Ok(Exported { saved: false })
    }
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
            test_blackhole,
            key_status,
            store_key,
            forget_key,
            start,
            stop,
            press,
            set_listen_sound,
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
