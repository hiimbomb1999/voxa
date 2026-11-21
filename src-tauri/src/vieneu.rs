use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tauri::{AppHandle, Manager};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

const BRIDGE: &str = include_str!("../sidecars/vieneu.py");

pub const SAMPLE_EN: &str = "Hello, this is Voxa. You are hearing the test voice for your meeting.";
pub const SAMPLE_VI: &str = "Xin chào, đây là giọng dịch của Voxa chạy trực tiếp trên máy Mac.";

fn directory(app: &AppHandle) -> Result<PathBuf> {
    Ok(app.path().app_local_data_dir()?.join("vieneu"))
}

pub fn voices(app: &AppHandle) -> Result<Vec<(String, String)>> {
    let root = directory(app)?;
    if !root.join("venv/bin/python").is_file() || !root.join("voices.json").is_file() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(
        root.join("voices.json"),
    )?)?)
}

pub fn validate(app: &AppHandle, name: &str) -> Result<()> {
    if !voices(app)?.iter().any(|(_, id)| id == name) {
        bail!(
            "Giọng VieNeu-TTS chưa được cài. Hãy chạy scripts/install-vieneu.sh rồi mở lại Voxa."
        );
    }
    Ok(())
}

pub struct Engine {
    _child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl Engine {
    pub async fn start(app: &AppHandle) -> Result<Self> {
        let root = directory(app)?;
        let mut child = Command::new(root.join("venv/bin/python"))
            .args(["-I", "-c", BRIDGE])
            .env("HF_HOME", root.join("models"))
            .env("HF_HUB_OFFLINE", "1")
            .env("HF_HUB_DISABLE_TELEMETRY", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Không mở được VieNeu-TTS.")?;

        let input = child
            .stdin
            .take()
            .context("Không mở được đường vào VieNeu-TTS.")?;
        let mut output = BufReader::new(
            child
                .stdout
                .take()
                .context("Không mở được đường ra VieNeu-TTS.")?,
        )
        .lines();
        let ready = timeout(Duration::from_secs(35), output.next_line()).await;
        if !matches!(ready, Ok(Ok(Some(line))) if line == "ready") {
            bail!("Không nạp được VieNeu-TTS. Hãy chạy lại scripts/install-vieneu.sh.");
        }
        Ok(Self {
            _child: child,
            input,
            output,
        })
    }

    pub async fn request(&mut self, text: &str, name: &str) -> Result<()> {
        let text: String = text.chars().take(1_024).collect();
        let mut request = serde_json::to_vec(&(text, name))?;
        request.push(b'\n');
        self.input
            .write_all(&request)
            .await
            .context("Không gửi được văn bản tới VieNeu-TTS.")?;
        Ok(())
    }

    pub async fn read(&mut self) -> Result<Option<Vec<u8>>> {
        let line = timeout(Duration::from_secs(8), self.output.next_line())
            .await
            .context("VieNeu-TTS phản hồi quá chậm.")??
            .ok_or_else(|| anyhow!("VieNeu-TTS đã ngừng chạy."))?;
        match line.as_str() {
            "" => Ok(None),
            "error" => bail!("VieNeu-TTS không tạo được giọng đọc."),
            _ => {
                let pcm = STANDARD.decode(line)?;
                if pcm.len() % 2 != 0 {
                    bail!("VieNeu-TTS trả về khung âm thanh không hợp lệ.");
                }
                Ok(Some(pcm))
            }
        }
    }
}
