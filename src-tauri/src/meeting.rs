use serde::Deserialize;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU8, Ordering::SeqCst},
        Mutex,
    },
    time::Instant,
};

const IDLE: u8 = 0;
const HOLDING: u8 = 1;
const REPLYING: u8 = 2;

#[derive(Default)]
pub struct PushToTalk {
    state: AtomicU8,
    released_at: Mutex<Option<Instant>>,
}

impl PushToTalk {
    pub fn press(&self) -> bool {
        self.state
            .compare_exchange(IDLE, HOLDING, SeqCst, SeqCst)
            .is_ok()
    }

    pub fn release(&self) -> bool {
        let mut released_at = self.released_at.lock().unwrap();
        let now = Instant::now();
        if self
            .state
            .compare_exchange(HOLDING, REPLYING, SeqCst, SeqCst)
            .is_err()
        {
            return false;
        }
        *released_at = Some(now);
        true
    }

    pub fn released_at(&self) -> Option<Instant> {
        *self.released_at.lock().unwrap()
    }

    pub fn reset(&self) {
        let mut released_at = self.released_at.lock().unwrap();
        *released_at = None;
        self.state.store(IDLE, SeqCst);
    }

    pub fn holding(&self) -> bool {
        self.state.load(SeqCst) == HOLDING
    }

    pub fn busy(&self) -> bool {
        self.state.load(SeqCst) != IDLE
    }

    pub fn replying(&self) -> bool {
        self.state.load(SeqCst) == REPLYING
    }
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MicSensitivity {
    Sensitive,
    #[default]
    Balanced,
    Noisy,
}

pub struct SpeechGate {
    threshold: f32,
    preroll: VecDeque<Vec<u8>>,
    consecutive: u8,
    opened: bool,
}

impl SpeechGate {
    pub fn new(sensitivity: MicSensitivity) -> Self {
        let threshold = match sensitivity {
            MicSensitivity::Sensitive => 0.006,
            MicSensitivity::Balanced => 0.012,
            MicSensitivity::Noisy => 0.026,
        };
        Self {
            threshold,
            preroll: VecDeque::with_capacity(5),
            consecutive: 0,
            opened: false,
        }
    }

    pub fn push(&mut self, pcm: Vec<u8>) -> Vec<Vec<u8>> {
        if self.opened {
            return vec![pcm];
        }
        if pcm.is_empty() {
            return Vec::new();
        }
        let samples = pcm.as_chunks::<2>().0;
        let energy = samples
            .iter()
            .map(|&bytes| {
                let sample = f32::from(i16::from_le_bytes(bytes)) / 32_768.0;
                sample * sample
            })
            .sum::<f32>()
            / samples.len() as f32;
        self.consecutive = if energy >= self.threshold * self.threshold {
            self.consecutive + 1
        } else {
            0
        };
        if self.preroll.len() == 5 {
            self.preroll.pop_front();
        }
        self.preroll.push_back(pcm);
        if self.consecutive < 2 {
            return Vec::new();
        }
        self.opened = true;
        self.preroll.drain(..).collect()
    }
}

#[cfg(test)]
mod timing_tests {
    use super::*;

    #[test]
    fn duplicate_release_does_not_move_the_start_of_the_measurement() {
        let talk = PushToTalk::default();
        assert!(talk.press());
        assert!(talk.release());
        let released = talk.released_at().unwrap();
        assert!(!talk.release());
        assert_eq!(talk.released_at(), Some(released));
        talk.reset();
        assert_eq!(talk.released_at(), None);
    }
}
