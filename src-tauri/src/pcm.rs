use crossbeam_queue::ArrayQueue;
use std::{
    sync::atomic::{AtomicU32, AtomicU64, Ordering::Relaxed},
    time::{Duration, Instant},
};

const ONSET_IDLE: u64 = 0;
const ONSET_WAITING: u64 = 1;
pub const AUDIBLE_FLOOR: f32 = 0.003;

pub struct Encoder {
    input_rate: u32,
    output_rate: u32,
    remaining: u32,
    sum: f32,
    chunk_bytes: usize,
    pending: Vec<u8>,
}

impl Encoder {
    pub fn new(input_rate: u32, chunk_samples: usize) -> Self {
        Self::with_output_rate(input_rate, 16_000, chunk_samples)
    }

    pub fn with_output_rate(input_rate: u32, output_rate: u32, chunk_samples: usize) -> Self {
        assert!(input_rate > 0 && output_rate > 0 && chunk_samples > 0);
        Self {
            input_rate,
            output_rate,
            remaining: input_rate,
            sum: 0.0,
            chunk_bytes: 2 * chunk_samples,
            pending: Vec::with_capacity(2 * chunk_samples),
        }
    }

    pub fn push(&mut self, value: f32) {
        let mut units = self.output_rate;
        while units > 0 {
            let used = units.min(self.remaining);
            self.sum += value * used as f32;
            self.remaining -= used;
            units -= used;
            if self.remaining == 0 {
                let mean = self.sum / self.input_rate as f32;
                let sample = (mean * 32_768.0).round() as i16;
                self.pending.extend_from_slice(&sample.to_le_bytes());
                self.remaining = self.input_rate;
                self.sum = 0.0;
            }
        }
    }

    pub fn take_chunk(&mut self) -> Option<Vec<u8>> {
        (self.pending.len() >= self.chunk_bytes).then(|| {
            let tail = self.pending.split_off(self.chunk_bytes);
            std::mem::replace(&mut self.pending, tail)
        })
    }

    pub fn flush(&mut self) -> Vec<u8> {
        self.remaining = self.input_rate;
        self.sum = 0.0;
        std::mem::take(&mut self.pending)
    }
}

pub struct Playback {
    rate: u32,
    samples: ArrayQueue<f32>,
    played_peak: AtomicU32,
    created_at: Instant,
    onset_ns: AtomicU64,
}

pub struct PlaybackResampler {
    previous: Option<f32>,
    phase: u32,
}

impl Playback {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            samples: ArrayQueue::new((rate * 3 / 4) as usize),
            played_peak: AtomicU32::new(0),
            created_at: Instant::now(),
            onset_ns: AtomicU64::new(ONSET_IDLE),
        }
    }

    pub fn resampler(&self) -> PlaybackResampler {
        PlaybackResampler {
            previous: None,
            phase: 0,
        }
    }

    pub fn enqueue(&self, bytes: &[u8], state: &mut PlaybackResampler) {
        for &sample in bytes.as_chunks::<2>().0 {
            let next = f32::from(i16::from_le_bytes(sample)) / 32_768.0;
            match state.previous.replace(next) {
                None => {
                    self.samples.force_push(next);
                    state.phase = 24_000;
                }
                Some(previous) => {
                    while state.phase <= self.rate {
                        let t = state.phase as f32 / self.rate as f32;
                        self.samples.force_push(previous + (next - previous) * t);
                        state.phase += 24_000;
                    }
                    state.phase -= self.rate;
                }
            }
        }
    }

    pub fn pop(&self) -> f32 {
        self.samples.pop().unwrap_or(0.0)
    }

    pub fn report_played_peak(&self, peak: f32) {
        self.played_peak.fetch_max(peak.to_bits(), Relaxed);
    }

    pub fn take_played_peak(&self) -> f32 {
        f32::from_bits(self.played_peak.swap(0, Relaxed))
    }

    pub fn track_onset(&self) {
        self.onset_ns.store(ONSET_WAITING, Relaxed);
    }

    pub fn awaiting_onset(&self) -> bool {
        self.onset_ns.load(Relaxed) == ONSET_WAITING
    }

    pub fn report_onset(&self, sample: f32, time: Instant) {
        if sample.abs() <= AUDIBLE_FLOOR {
            return;
        }
        let elapsed = time.saturating_duration_since(self.created_at).as_nanos() as u64;
        let _ = self.onset_ns.compare_exchange(
            ONSET_WAITING,
            elapsed.saturating_add(2),
            Relaxed,
            Relaxed,
        );
    }

    pub fn first_output(&self) -> Option<Instant> {
        let elapsed = self.onset_ns.load(Relaxed).checked_sub(2)?;
        Some(self.created_at + Duration::from_nanos(elapsed))
    }

    pub fn clear(&self) {
        while self.samples.pop().is_some() {}
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn buffered_millis(&self) -> usize {
        self.len() * 1_000 / self.rate as usize
    }
}

pub struct OriginalAudio {
    rate: AtomicU32,
    frames: ArrayQueue<[f32; 2]>,
}

impl Default for OriginalAudio {
    fn default() -> Self {
        Self {
            rate: AtomicU32::new(0),
            frames: ArrayQueue::new(24_576),
        }
    }
}

impl OriginalAudio {
    pub fn set_rate(&self, rate: u32) {
        self.rate.store(rate, Relaxed);
    }

    pub fn push(&self, frame: [f32; 2]) {
        self.frames.force_push(frame);
    }
}

pub struct OriginalReader {
    output_rate: u32,
    input_rate: u32,
    phase: u32,
    pair: Option<[[f32; 2]; 2]>,
}

impl OriginalReader {
    pub fn new(output_rate: u32) -> Self {
        Self {
            output_rate,
            input_rate: 0,
            phase: 0,
            pair: None,
        }
    }

    pub fn start_buffer(&mut self, audio: &OriginalAudio) {
        let rate = audio.rate.load(Relaxed);
        if rate != self.input_rate || audio.frames.len() > (rate / 8) as usize {
            self.input_rate = rate;
            self.pair = None;
            while audio.frames.len() > (rate / 22) as usize {
                audio.frames.pop();
            }
        }
    }

    pub fn next(&mut self, audio: &OriginalAudio) -> [f32; 2] {
        let [mut previous, mut current] = match self.pair {
            Some(pair) => pair,
            None => {
                // Refill a short buffer after underruns before restarting playback.
                if self.input_rate == 0 || audio.frames.len() < (self.input_rate / 50) as usize {
                    return [0.0; 2];
                }
                let Some(previous) = audio.frames.pop() else {
                    return [0.0; 2];
                };
                let Some(current) = audio.frames.pop() else {
                    return [0.0; 2];
                };
                self.phase = 0;
                [previous, current]
            }
        };

        let fraction = self.phase as f32 / self.output_rate as f32;
        let frame = [
            previous[0] + (current[0] - previous[0]) * fraction,
            previous[1] + (current[1] - previous[1]) * fraction,
        ];

        self.phase += self.input_rate;
        while self.phase >= self.output_rate {
            self.phase -= self.output_rate;
            previous = current;
            let Some(next) = audio.frames.pop() else {
                self.pair = None;
                return frame;
            };
            current = next;
        }
        self.pair = Some([previous, current]);
        frame
    }
}

pub struct DuckingMixer {
    gain: f32,
    hold_samples: u32,
    hold_remaining: u32,
    attack: f32,
    release: f32,
}

impl DuckingMixer {
    pub fn new(rate: u32) -> Self {
        Self {
            gain: 1.0,
            hold_samples: rate * 3 / 10,
            hold_remaining: 0,
            attack: 1.0 - (-1.0 / (rate as f32 * 0.006)).exp(),
            release: 1.0 - (-1.0 / (rate as f32 * 0.18)).exp(),
        }
    }

    pub fn mix(&mut self, original: [f32; 2], translation: f32) -> [f32; 2] {
        if translation.abs() > AUDIBLE_FLOOR {
            self.hold_remaining = self.hold_samples;
        } else {
            self.hold_remaining = self.hold_remaining.saturating_sub(1);
        }
        let (target, factor) = if self.hold_remaining > 0 {
            (0.08, self.attack)
        } else {
            (1.0, self.release)
        };
        self.gain += (target - self.gain) * factor;
        let gain = self.gain * (1.0 - translation.abs().min(1.0));
        original.map(|sample| (sample * gain + translation).clamp(-1.0, 1.0))
    }
}
