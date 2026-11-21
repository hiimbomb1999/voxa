use crate::provider::{self, Socket};
use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::{timeout, Instant};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{client::IntoClientRequest, http, protocol::WebSocketConfig, Error, Message},
};
use voxa::protocol::Event;

const ACCOUNT: &str = "soniox-api-key";
const STT: &str = "wss://stt-rt.soniox.com/transcribe-websocket";
const TTS: &str = "wss://tts-rt.soniox.com/tts-websocket";
const STT_MODEL: &str = "stt-rt-v6";
const TTS_MODEL: &str = "tts-rt-v2";
const STREAM_ID: &str = "voxa";
const SUPPORTED: &str = "af sq ar az eu be bn bs bg ca zh hr cs da nl en et fi fr gl de el gu he hi hu id it ja kn kk ko lv lt mk ms ml mr no fa pl pt pa ro ru sr sk sl es sw sv tl ta te th tr uk ur vi cy";

pub fn languages() -> impl Iterator<Item = &'static str> {
    SUPPORTED.split_whitespace()
}

pub fn supports_language(code: &str) -> bool {
    languages().any(|item| item == code)
}

pub fn load_key() -> Result<Option<String>> {
    crate::credentials::load(ACCOUNT)
}

pub fn store_key(key: &str) -> Result<()> {
    let key = key.trim();
    if !(12..=512).contains(&key.len()) || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        bail!("Soniox API key không đúng định dạng.");
    }
    crate::credentials::store(ACCOUNT, key)
}

pub fn forget_key() -> Result<()> {
    crate::credentials::forget(ACCOUNT)
}

pub async fn check_key(key: &str) -> Result<()> {
    let response = provider::client()?
        .get("https://api.soniox.com/v1/models")
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| anyhow!("Không kết nối được với Soniox."))?;
    if !response.status().is_success() {
        return Err(ServiceError(u64::from(response.status().as_u16())).into());
    }
    let models: Value = response
        .json()
        .await
        .map_err(|_| anyhow!("Không đọc được danh sách model Soniox."))?;
    if !models["models"]
        .as_array()
        .is_some_and(|models| models.iter().any(|model| model["id"] == STT_MODEL))
    {
        bail!("Soniox chưa trả về model {STT_MODEL} cho dự án này.");
    }
    Ok(())
}

#[derive(Debug)]
struct ServiceError(u64);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = self.0;
        match status {
            401 | 403 => write!(
                f,
                "Soniox từ chối API key hoặc quyền dùng model ({status})."
            ),
            402 => f.write_str("Dự án Soniox đã hết số dư hoặc ngân sách (402)."),
            429 => f.write_str("Dự án Soniox đã đạt giới hạn sử dụng (429)."),
            _ => write!(f, "Soniox trả về lỗi ({status})."),
        }
    }
}

impl std::error::Error for ServiceError {}

pub fn voice_access_denied(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<ServiceError>(),
        Some(ServiceError(401 | 402 | 403 | 429))
    )
}

fn request(url: &str, key: &str) -> Result<http::Request<()>> {
    let mut request = url
        .into_client_request()
        .map_err(|_| anyhow!("Không tạo được kết nối Soniox."))?;
    let mut authorization = http::HeaderValue::from_str(&format!("Bearer {key}"))
        .map_err(|_| anyhow!("Soniox API key không hợp lệ."))?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(http::header::AUTHORIZATION, authorization);
    Ok(request)
}

async fn connect(url: &str, key: &str) -> Result<Socket> {
    let request = request(url, key)?;
    let config = WebSocketConfig::default().max_message_size(Some(2 * 1024 * 1024));
    match timeout(
        Duration::from_secs(10),
        connect_async_with_config(request, Some(config), true),
    )
    .await
    {
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(Error::Http(response))) => {
            Err(ServiceError(u64::from(response.status().as_u16())).into())
        }
        _ => bail!("Không mở được kết nối Soniox. Kiểm tra đường truyền."),
    }
}

pub async fn send(socket: &mut Socket, message: Message) -> Result<()> {
    timeout(Duration::from_secs(2), socket.send(message))
        .await
        .map_err(|_| anyhow!("Đường gửi tới Soniox bị chậm."))?
        .map_err(|_| anyhow!("Mất kết nối khi gửi tới Soniox."))
}

fn text(value: Value) -> Message {
    Message::Text(value.to_string().into())
}

async fn receive(socket: &mut Socket) -> Result<Value> {
    loop {
        return match socket.next().await {
            Some(Ok(Message::Text(frame))) => {
                let result: Value = serde_json::from_str(&frame)
                    .map_err(|_| anyhow!("Soniox trả về sự kiện không hợp lệ."))?;
                if let Some(code) = result["error_code"].as_u64() {
                    return Err(ServiceError(code).into());
                }
                Ok(result)
            }
            Some(Ok(Message::Pong(_))) => Ok(Value::Null),
            Some(Ok(Message::Ping(_))) => {
                timeout(Duration::from_secs(2), socket.flush())
                    .await
                    .map_err(|_| anyhow!("Soniox ngừng phản hồi."))?
                    .map_err(|_| anyhow!("Mất kết nối với Soniox."))?;
                Ok(Value::Null)
            }
            Some(Ok(Message::Frame(_))) => continue,
            _ => bail!("Soniox đã đóng kết nối."),
        };
    }
}

fn stt_setup(language: &str, talk: bool) -> Value {
    let mut config = json!({
        "model": STT_MODEL,
        "audio_format": "pcm_s16le",
        "sample_rate": 16000,
        "num_channels": 1,
        "enable_language_identification": true,
        "enable_endpoint_detection": !talk,
        "translation": { "type": "one_way", "target_language": language }
    });
    if talk {
        config["language_hints"] = json!(["vi"]);
    } else {
        config["max_endpoint_delay_ms"] = json!(500);
    }
    config
}

pub async fn open_stt(key: &str, language: &str, talk: bool) -> Result<Socket> {
    let mut socket = connect(STT, key).await?;
    send(&mut socket, text(stt_setup(language, talk))).await?;
    Ok(socket)
}

pub async fn finalize(socket: &mut Socket) -> Result<()> {
    send(socket, text(json!({"type":"finalize"}))).await
}

pub async fn keepalive(socket: &mut Socket) -> Result<()> {
    send(socket, text(json!({"type":"keepalive"}))).await?;
    send(socket, Message::Ping(Vec::new().into())).await
}

fn stt_events(message: &Value) -> Vec<Event> {
    let Some(tokens) = message["tokens"].as_array() else {
        return Vec::new();
    };
    let mut stable = [String::new(), String::new()];
    let mut provisional = [String::new(), String::new()];
    let mut ended = false;
    for token in tokens {
        let Some(value) = token["text"].as_str() else {
            continue;
        };
        let final_token = token["is_final"].as_bool() == Some(true);
        if matches!(value, "<end>" | "<fin>") {
            ended |= final_token;
            continue;
        }
        let translation = token["translation_status"] == "translation";
        let parts = if final_token {
            &mut stable
        } else {
            &mut provisional
        };
        parts[usize::from(translation)].push_str(value);
    }

    let mut events: Vec<_> = stable
        .into_iter()
        .enumerate()
        .filter(|(_, text)| !text.is_empty())
        .map(|(index, text)| Event::Caption {
            text,
            is_translation: index == 1,
            finished: false,
        })
        .collect();
    if ended || message["finished"] == true {
        events.push(Event::TurnComplete);
    }
    events.push(Event::ProvisionalCaptions(provisional));
    events
}

pub async fn receive_stt(socket: &mut Socket) -> Result<(Vec<Event>, bool)> {
    let message = receive(socket).await?;
    Ok((stt_events(&message), message["finished"] == true))
}

pub struct Voice {
    key: String,
    language: String,
    socket: Option<(Socket, Instant)>,
}

impl Voice {
    pub fn new(key: String, language: String) -> Self {
        Self {
            key,
            language,
            socket: None,
        }
    }

    pub async fn start(&mut self, phrase: &str) -> Result<Socket> {
        let mut socket = match self.socket.take() {
            Some((socket, used)) if used.elapsed() < Duration::from_secs(30) => socket,
            _ => connect(TTS, &self.key).await?,
        };
        send(
            &mut socket,
            text(json!({
                "model": TTS_MODEL, "language": self.language, "voice": "Adrian",
                "audio_format": "pcm_s16le", "sample_rate": 24000, "stream_id": STREAM_ID
            })),
        )
        .await?;
        send(
            &mut socket,
            text(json!({"stream_id":STREAM_ID,"text":phrase,"text_end":true})),
        )
        .await?;
        Ok(socket)
    }

    pub fn reuse(&mut self, socket: Socket) {
        self.socket = Some((socket, Instant::now()));
    }
}

pub async fn read_voice(socket: &mut Socket) -> Result<Option<Vec<u8>>> {
    timeout(Duration::from_secs(10), async {
        loop {
            let message = receive(socket).await?;
            if message["stream_id"] != STREAM_ID {
                continue;
            }
            if message["terminated"] == true {
                return Ok(None);
            }
            if let Some(audio) = message["audio"].as_str() {
                let pcm = STANDARD
                    .decode(audio)
                    .map_err(|_| anyhow!("Soniox trả về âm thanh không hợp lệ."))?;
                if pcm.len() % 2 != 0 {
                    bail!("Soniox trả về khung âm thanh không hợp lệ.");
                }
                if !pcm.is_empty() {
                    return Ok(Some(pcm));
                }
            }
        }
    })
    .await
    .map_err(|_| anyhow!("Giọng đọc Soniox phản hồi quá chậm."))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stt_auth_and_input_configuration_match_the_live_api() {
        let request = request(STT, "test-project-key").unwrap();
        assert_eq!(
            request.headers()[http::header::AUTHORIZATION],
            "Bearer test-project-key"
        );
        assert!(request.headers()[http::header::AUTHORIZATION].is_sensitive());
        assert!(request.uri().query().is_none());
        let config = stt_setup("vi", false);
        assert_eq!(config["model"], STT_MODEL);
        assert_eq!(config["audio_format"], "pcm_s16le");
        assert_eq!(config["translation"]["target_language"], "vi");
        assert_eq!(stt_setup("en", true)["enable_endpoint_detection"], false);
    }

    #[tokio::test]
    async fn reads_translated_tokens_and_streamed_pcm_over_websockets() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server =
            tokio::spawn(async move {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut socket = tokio_tungstenite::accept_async(tcp).await.unwrap();
                assert!(matches!(socket.next().await, Some(Ok(Message::Binary(_)))));
                assert_eq!(
                    socket.next().await.unwrap().unwrap(),
                    text(json!({"type":"finalize"}))
                );
                socket.send(text(json!({"tokens":[
                {"text":"This is a test.","translation_status":"translation","is_final":true},
                {"text":"<fin>","is_final":true}
            ]}))).await.unwrap();
                socket
                    .send(text(
                        json!({"audio":"AQA=","stream_id":STREAM_ID,"audio_end":true}),
                    ))
                    .await
                    .unwrap();
                socket
                    .send(text(json!({"stream_id":STREAM_ID,"terminated":true})))
                    .await
                    .unwrap();
            });
        let mut socket = connect(&format!("ws://{addr}"), "test-credential")
            .await
            .unwrap();
        send(&mut socket, Message::Binary(vec![0; 640].into()))
            .await
            .unwrap();
        finalize(&mut socket).await.unwrap();
        let (events, finished) = receive_stt(&mut socket).await.unwrap();
        assert!(!finished);
        assert!(events.contains(&Event::TurnComplete));
        assert!(events.contains(&Event::Caption {
            text: "This is a test.".into(),
            is_translation: true,
            finished: false,
        }));
        assert_eq!(read_voice(&mut socket).await.unwrap(), Some(vec![1, 0]));
        assert_eq!(read_voice(&mut socket).await.unwrap(), None);
        server.await.unwrap();
    }

    #[test]
    fn separates_final_translation_from_provisional_text_and_control_tokens() {
        let events = stt_events(&json!({"tokens": [
            {"text":"Hello","is_final":true,"translation_status":"original"},
            {"text":"Xin chào","is_final":true,"translation_status":"translation"},
            {"text":"<end>","is_final":true},
            {"text":" bạn","is_final":false,"translation_status":"translation"}
        ]}));
        assert_eq!(
            events,
            vec![
                Event::Caption {
                    text: "Hello".into(),
                    is_translation: false,
                    finished: false
                },
                Event::Caption {
                    text: "Xin chào".into(),
                    is_translation: true,
                    finished: false
                },
                Event::TurnComplete,
                Event::ProvisionalCaptions([String::new(), " bạn".into()])
            ]
        );
        assert!(stt_events(&Value::Null).is_empty());
    }
}
