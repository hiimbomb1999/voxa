use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Translate,
    Assistant,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Ready,
    RotateAfter(Duration),
    Caption {
        text: String,
        is_translation: bool,
        finished: bool,
    },
    Audio(Vec<u8>),
    Interrupted,
    TurnComplete,
}

pub fn setup(mode: Mode, language: &str) -> Value {
    match mode {
        Mode::Translate => json!({ "setup": {
            "model": "models/gemini-3.5-live-translate-preview",
            "inputAudioTranscription": {},
            "outputAudioTranscription": {},
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "translationConfig": {"targetLanguageCode":language, "echoTargetLanguage":false}
            }
        }}),
        Mode::Assistant => json!({ "setup": {
            "model": "models/gemini-3.8-live",
            "systemInstruction": {"parts":[{"text":"Bạn là trợ lý giọng nói Voxa. Trả lời ngắn gọn, cùng ngôn ngữ với người dùng."}]},
            "inputAudioTranscription": {},
            "outputAudioTranscription": {},
            "generationConfig": {"responseModalities":["AUDIO"]}
        }}),
    }
}

pub fn input(bytes: &[u8]) -> Value {
    json!({"realtimeInput":{"audio":{"mimeType":"audio/pcm;rate=16000","data":STANDARD.encode(bytes)}}})
}

pub fn input_end() -> Value {
    json!({"realtimeInput":{"audioStreamEnd":true}})
}

pub fn parse(response: &Value) -> Vec<Event> {
    let mut events = Vec::new();
    if response.get("setupComplete").is_some() {
        events.push(Event::Ready);
    }
    if let Some(go_away) = response.get("goAway") {
        let seconds = go_away["timeLeft"]
            .as_str()
            .and_then(|value| value.strip_suffix('s'))
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .unwrap_or(0.0);
        events.push(Event::RotateAfter(Duration::from_secs_f64(
            (seconds - 1.0).clamp(0.0, 600.0),
        )));
    }
    if let Some(content) = response.get("serverContent") {
        if content["interrupted"].as_bool() == Some(true) {
            events.push(Event::Interrupted);
        }
        for (key, is_translation) in [("inputTranscription", false), ("outputTranscription", true)]
        {
            if let Some(transcript) = content.get(key) {
                let text = transcript["text"].as_str().unwrap_or_default();
                let finished = transcript["finished"].as_bool().unwrap_or(false);
                if !text.is_empty() || finished {
                    events.push(Event::Caption {
                        text: text.to_owned(),
                        is_translation,
                        finished,
                    });
                }
            }
        }
        if let Some(parts) = content["modelTurn"]["parts"].as_array() {
            for part in parts {
                let inline = &part["inlineData"];
                let mime = inline["mimeType"].as_str().unwrap_or_default();
                if mime.starts_with("audio/pcm") && !mime.contains(";rate=")
                    || mime == "audio/pcm;rate=24000"
                {
                    if let Some(data) = inline["data"].as_str() {
                        if let Ok(bytes) = STANDARD.decode(data) {
                            if !bytes.is_empty() && bytes.len() % 2 == 0 {
                                events.push(Event::Audio(bytes));
                            }
                        }
                    }
                }
            }
        }
        if content["turnComplete"].as_bool() == Some(true) {
            events.push(Event::TurnComplete);
        }
    }
    events
}
