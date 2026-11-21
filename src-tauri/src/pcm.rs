use crossbeam_queue::ArrayQueue;

pub struct Encoder {
    input_rate: u32,
    remaining: u32,
    sum: f32,
    chunk_bytes: usize,
    pending: Vec<u8>,
}

impl Encoder {
    pub fn new(input_rate: u32, chunk_samples: usize) -> Self {
        assert!(input_rate > 0 && chunk_samples > 0);
        Self {
            input_rate,
            remaining: input_rate,
            sum: 0.0,
            chunk_bytes: 2 * chunk_samples,
            pending: Vec::with_capacity(2 * chunk_samples),
        }
    }

    pub fn push(&mut self, value: f32) {
        let mut units = 16_000;
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
