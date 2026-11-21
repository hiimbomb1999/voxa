use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::json;
use voxa::{
    pcm::{Encoder, Playback},
    protocol::{self, Event, Mode},
};

#[test]
fn translate_setup_uses_the_documented_wire_schema() {
    assert_eq!(
        protocol::setup(Mode::Translate, "vi"),
        json!({
            "setup": {
                "model": "models/gemini-3.5-live-translate-preview",
                "inputAudioTranscription": {},
                "outputAudioTranscription": {},
                "generationConfig": {
                    "responseModalities": ["AUDIO"],
                    "translationConfig": { "targetLanguageCode": "vi", "echoTargetLanguage": false }
                }
            }
        })
    );
}

#[test]
fn parses_setup_expiry_and_independent_captions() {
    assert!(matches!(
        protocol::parse(&json!({"setupComplete": {}})).as_slice(),
        [Event::Ready]
    ));
    assert!(
        matches!(protocol::parse(&json!({"goAway": {"timeLeft": "12.5s"}})).as_slice(), [Event::RotateAfter(time)] if time.as_millis() == 11_500)
    );
    let message = json!({"serverContent": {
        "inputTranscription": {"text":"Good", "finished":false},
        "outputTranscription": {"text":"Chào", "finished":true},
        "modelTurn":{"parts":[{"inlineData":{"mimeType":"audio/pcm;rate=24000","data": STANDARD.encode([0,1,2,3])}}]}
    }});
    assert_eq!(
        protocol::parse(&message),
        [
            Event::Caption {
                text: "Good".into(),
                is_translation: false,
                finished: false
            },
            Event::Caption {
                text: "Chào".into(),
                is_translation: true,
                finished: true
            },
            Event::Audio(vec![0, 1, 2, 3]),
        ]
    );
}

#[test]
fn encoder_keeps_fractional_progress_across_fragments_and_flushes_tail() {
    let mut encoder = Encoder::new(44_100, 1_600);
    for _ in 0..4_410 {
        encoder.push(0.25);
    }
    let full_chunk = encoder.take_chunk().unwrap();
    assert_eq!(full_chunk.len(), 3_200);
    assert!(full_chunk
        .as_chunks::<2>()
        .0
        .iter()
        .all(|sample| *sample == [0, 32]));
    for _ in 0..441 {
        encoder.push(0.25);
    }
    assert!(encoder.take_chunk().is_none());
    assert_eq!(encoder.flush().len(), 320);
}

#[test]
fn playback_interpolates_and_drops_old_audio_at_capacity() {
    let playback = Playback::new(48_000);
    playback.enqueue(&[0, 0, 0, 64, 0, 0], &mut playback.resampler());
    assert_eq!(playback.pop(), 0.0);
    assert!((playback.pop() - 0.25).abs() < 0.0001);
    assert!((playback.pop() - 0.5).abs() < 0.0001);
    playback.enqueue(&[255, 127].repeat(48_000), &mut playback.resampler());
    assert!(playback.len() <= 36_000);
    playback.clear();
    assert_eq!(playback.pop(), 0.0);
}

#[test]
fn playback_level_reports_only_peaks_observed_by_the_output_callback() {
    let playback = Playback::new(24_000);
    playback.enqueue(&[0xff, 0x7f], &mut playback.resampler());
    assert_eq!(playback.take_played_peak(), 0.0);
    playback.report_played_peak(playback.pop().abs());
    playback.report_played_peak(0.25);
    assert!(playback.take_played_peak() > 0.99);
    assert_eq!(playback.take_played_peak(), 0.0);
}

#[test]
fn playback_onset_ignores_silence_and_reports_only_the_first_output() {
    use std::time::{Duration, Instant};

    let playback = Playback::new(24_000);
    playback.track_onset();
    let callback = Instant::now();
    playback.report_onset(0.0, callback);
    assert_eq!(playback.first_output(), None);
    let first = callback + Duration::from_millis(20);
    playback.report_onset(0.02, first);
    playback.report_onset(0.8, first + Duration::from_millis(20));
    assert_eq!(playback.first_output(), Some(first));
    playback.track_onset();
    assert_eq!(playback.first_output(), None);
}

#[test]
fn loopback_encoder_preserves_the_length_at_playback_sample_rate() {
    let mut encoder = Encoder::with_output_rate(48_000, 24_000, 1_600);
    for sample in [0.25, -0.25].into_iter().cycle().take(48_000) {
        encoder.push(sample);
    }
    assert_eq!(encoder.flush().len(), 48_000);
}

#[test]
fn translated_speech_ducks_original_and_restores_stereo() {
    use voxa::pcm::DuckingMixer;

    let mut mixer = DuckingMixer::new(48_000);
    let source = [0.5, -0.25];
    assert_eq!(mixer.mix(source, 0.0), source);
    let mut mixed = mixer.mix(source, 0.3);
    for _ in 0..4_800 {
        mixed = mixer.mix(source, 0.3);
    }
    assert!(mixed[0] < 0.39 && mixed[1] > 0.26);
    assert!(mixer.mix(source, 0.0)[0] < 0.12);
    for _ in 0..12_000 {
        mixer.mix(source, 0.0);
    }
    assert!(mixer.mix(source, 0.0)[0] < 0.2);
    for _ in 0..72_000 {
        mixer.mix(source, 0.0);
    }
    let restored = mixer.mix(source, 0.0);
    assert!((restored[0] - source[0]).abs() < 0.01);
    assert!((restored[1] - source[1]).abs() < 0.01);
}

#[test]
fn quiet_translation_becomes_clear_against_loud_original_audio() {
    let mut mixer = voxa::pcm::DuckingMixer::new(48_000);
    let original = [0.82, -0.82];
    let voice = 0.07;
    for _ in 0..1_200 {
        mixer.mix(original, voice);
    }
    let mixed = mixer.mix(original, voice);
    assert!((mixed[0] - voice).abs() < 0.1);
    assert!((mixed[1] - voice).abs() < 0.1);
}

#[test]
fn original_audio_resamples_stereo_and_recovers_after_underrun() {
    use voxa::pcm::{OriginalAudio, OriginalReader};

    let audio = OriginalAudio::default();
    audio.set_rate(44_100);
    let mut reader = OriginalReader::new(48_000);
    reader.start_buffer(&audio);
    assert_eq!(reader.next(&audio), [0.0; 2]);
    for _ in 0..1_800 {
        audio.push([0.25, -0.125]);
    }
    for _ in 0..1_200 {
        assert_eq!(reader.next(&audio), [0.25, -0.125]);
    }
    for _ in 0..1_200 {
        reader.next(&audio);
    }
    assert_eq!(reader.next(&audio), [0.0; 2]);
    for _ in 0..1_800 {
        audio.push([-0.125, 0.25]);
    }
    assert_eq!(reader.next(&audio), [-0.125, 0.25]);
}
