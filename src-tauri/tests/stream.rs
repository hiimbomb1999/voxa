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
