use super::Capture;
use anyhow::{anyhow, Context, Result};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering::Relaxed},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat};

const RATE: u32 = 48_000;

pub struct Tap {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Tap {
    pub fn open(capture: Arc<Capture>, failed: Arc<AtomicBool>) -> Result<(Self, u32)> {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let (ready, result) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("voxa-wasapi".into())
            .spawn(move || {
                if let Err(error) = capture_system(&capture, &stop_thread, &ready) {
                    let _ = ready.try_send(Err(error));
                    failed.store(true, Relaxed);
                }
            })?;

        let startup = result.recv_timeout(Duration::from_secs(5));
        match startup {
            Ok(Ok(())) => Ok((
                Self {
                    stop,
                    thread: Some(thread),
                },
                RATE,
            )),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                stop.store(true, Relaxed);
                Err(anyhow!("Windows chưa mở được bộ thu âm thanh hệ thống."))
            }
        }
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        self.stop.store(true, Relaxed);
        // The audio loop wakes at least every 100 ms to observe shutdown.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn capture_system(
    capture: &Capture,
    stop: &AtomicBool,
    ready: &mpsc::SyncSender<Result<()>>,
) -> Result<()> {
    wasapi::initialize_mta().ok()?;
    let _application = ComApartment;
    // Exclude Voxa and child processes so its own translated voice is not translated again.
    let mut audio = AudioClient::new_application_loopback_client(std::process::id(), false)
        .context("Không mở được WASAPI loopback. Tính năng này cần Windows 11.")?;
    let format = WaveFormat::new(32, 32, SampleType::Float, RATE as usize, 2, None);
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 0,
    };
    audio.initialize_client(&format, Direction::Capture, mode, None)?;
    let event = audio.set_get_eventhandle()?;
    let source = audio.get_audiocaptureclient()?;
    audio.start_stream()?;
    if ready.send(Ok(())).is_err() {
        audio.stop_stream()?;
        return Ok(());
    }

    let mut packet = VecDeque::with_capacity(8_192);
    while !stop.load(Relaxed) {
        let _ = event.wait_for_event(100);
        while !stop.load(Relaxed) && source.get_next_packet_size()?.unwrap_or(0) > 0 {
            source.read_from_device_to_deque(&mut packet)?;
            let samples = packet.make_contiguous().as_chunks::<4>().0;
            capture.write_mono(samples.as_chunks::<2>().0.iter().map(|[left, right]| {
                (f32::from_le_bytes(*left) + f32::from_le_bytes(*right)) * 0.5
            }));
            packet.clear();
        }
    }
    audio.stop_stream()?;
    Ok(())
}

struct ComApartment;

impl Drop for ComApartment {
    fn drop(&mut self) {
        wasapi::deinitialize();
    }
}
