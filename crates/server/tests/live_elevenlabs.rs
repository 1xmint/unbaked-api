//! Real calls to ElevenLabs, run by hand; they cost under two cents:
//!
//! cargo test -p unbaked-api --test live_elevenlabs -- --ignored --nocapture --test-threads 1
//!
//! Needs ELEVENLABS_API_KEY in `.env` at the repo root (or the environment).
//! Music needs a paid ElevenLabs plan.

use unbaked_api::elevenlabs::ElevenLabs;
use unbaked_api::providers::{MusicJob, Sounds, SpeechJob, VoiceModel, is_mp3};

fn client() -> ElevenLabs {
    let _ = dotenvy::from_filename("../../.env");
    let key = std::env::var("ELEVENLABS_API_KEY").expect("ELEVENLABS_API_KEY is not set");
    ElevenLabs::new(&key).unwrap()
}

#[tokio::test]
#[ignore = "calls ElevenLabs and costs money"]
async fn elevenlabs_says_a_line_in_the_first_voice() {
    let elevenlabs = client();
    let voices = elevenlabs.voices().await.unwrap();
    println!("{} voices; first: {:?}", voices.len(), voices.first());
    let voice = voices.first().expect("the account has no voices");
    let job = SpeechJob {
        text: "Fresh bread, every morning.".to_owned(),
        voice_id: voice.voice_id.clone(),
        model: VoiceModel::FlashV2_5,
        language_code: None,
        seed: None,
    };
    let bytes = elevenlabs.speech(job).await.unwrap();
    assert!(is_mp3(&bytes));
    let path = std::env::temp_dir().join("unbaked-live-speech.mp3");
    std::fs::write(&path, &bytes).unwrap();
    println!("{} bytes written to {}", bytes.len(), path.display());
}

#[tokio::test]
#[ignore = "calls ElevenLabs and costs money"]
async fn elevenlabs_makes_five_seconds_of_music() {
    let job = MusicJob {
        prompt: "quiet warm solo piano, slow".to_owned(),
        length_ms: 5_000,
        instrumental: true,
        seed: None,
    };
    let bytes = client().music(job).await.unwrap();
    assert!(is_mp3(&bytes));
    let path = std::env::temp_dir().join("unbaked-live-music.mp3");
    std::fs::write(&path, &bytes).unwrap();
    println!("{} bytes written to {}", bytes.len(), path.display());
}
