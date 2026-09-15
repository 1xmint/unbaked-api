//! What paid work costs, in millionths of a USDC (USDC has six decimals).
//!
//! Test-network prices only need to be roughly right. File work is priced by
//! the time it should take, from layer 1's `estimate`: about 0.66 ms per work
//! unit, the one rate measured so far (an image on the layer 1 bench). Measure
//! sound and video before real money.
//!
//! AI calls are priced from what the provider will charge us (the `cost`,
//! which counts against the daily cap) plus a margin. OpenAI bills pictures by
//! token: $5 per million text tokens in, $8 per million picture tokens in, $30
//! per million picture tokens out (pricing page, 2026-09-14).

use crate::providers::{Quality, VoiceModel};

/// The least any paid call costs: half a cent.
pub const FLOOR: u64 = 5_000;

/// Milliseconds of rendering per work unit.
pub const MS_PER_WORK_UNIT: f64 = 0.66;

/// What a millisecond of rendering costs.
pub const PER_MS: u64 = 1;

/// An edit changes JSON and packs files; it costs the floor.
pub const EDIT: u64 = FLOOR;

/// Our margin over provider cost, in percent.
pub const MARGIN_PERCENT: u64 = 25;

/// What one 1024×1024 picture costs us from OpenAI at each quality. From a
/// third-party calculation that matches the token prices; the live test
/// prints real token counts to check it.
pub const PICTURE_1024: [(Quality, u64); 3] = [
    (Quality::Low, 6_000),
    (Quality::Medium, 53_000),
    (Quality::High, 211_000),
];

/// Text in: $5 per million tokens, counting a token for every 3 characters
/// (English runs nearer 4).
pub const PER_PROMPT_TOKEN: u64 = 5;
pub const CHARS_PER_TOKEN: u64 = 3;

/// A source picture in an edit: 6,000 picture tokens at $8 per million. Not
/// measured; the live test checks it.
pub const PER_SOURCE_PICTURE: u64 = 48_000;

/// ElevenLabs speech per character: $0.10 per 1,000 for multilingual v2 and
/// v3, $0.05 for flash v2.5 (API pricing page, 2026-09-14).
pub const SPEECH_PER_CHAR: [(VoiceModel, u64); 3] = [
    (VoiceModel::MultilingualV2, 100),
    (VoiceModel::V3, 100),
    (VoiceModel::FlashV2_5, 50),
];

/// ElevenLabs music: $0.15 per minute, which is 2.5 millionths per millisecond.
pub const MUSIC_PER_MINUTE: u64 = 150_000;

/// The price for a provider cost: cost plus margin, never under the floor.
pub fn with_margin(cost: u64) -> u64 {
    cost.saturating_mul(100 + MARGIN_PERCENT)
        .div_ceil(100)
        .max(FLOOR)
}

/// What OpenAI will charge us for one picture: output by size and quality,
/// the prompt, and any source pictures.
pub fn picture_cost(
    quality: Quality,
    width: u32,
    height: u32,
    prompt_chars: usize,
    sources: usize,
) -> u64 {
    let base = PICTURE_1024
        .iter()
        .find(|(q, _)| *q == quality)
        .map_or(0, |(_, cost)| *cost);
    let pixels = u64::from(width) * u64::from(height);
    let output = (base * pixels).div_ceil(1024 * 1024);
    let prompt = (prompt_chars as u64).div_ceil(CHARS_PER_TOKEN) * PER_PROMPT_TOKEN;
    output + prompt + sources as u64 * PER_SOURCE_PICTURE
}

/// What ElevenLabs will charge us to say `chars` characters.
pub fn speech_cost(model: VoiceModel, chars: usize) -> u64 {
    let per_char = SPEECH_PER_CHAR
        .iter()
        .find(|(m, _)| *m == model)
        .map_or(0, |(_, cost)| *cost);
    per_char * chars as u64
}

/// What ElevenLabs will charge us for `length_ms` of music.
pub fn music_cost(length_ms: u32) -> u64 {
    (u64::from(length_ms) * MUSIC_PER_MINUTE).div_ceil(60_000)
}

/// The price of file work of this size.
pub fn file_work(work_units: u64) -> u64 {
    let ms = (work_units as f64 * MS_PER_WORK_UNIT).ceil() as u64;
    ms.saturating_mul(PER_MS).max(FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_work_costs_the_floor_and_big_work_scales() {
        assert_eq!(file_work(0), FLOOR);
        assert_eq!(file_work(1_000), FLOOR);
        assert_eq!(file_work(100_000), 66_000);
        assert!(file_work(u64::MAX) > FLOOR);
    }

    #[test]
    fn a_picture_costs_its_size_and_quality_plus_the_margin() {
        assert_eq!(picture_cost(Quality::Medium, 1024, 1024, 0, 0), 53_000);
        // Twice the pixels, twice the output; 30 characters is 10 tokens.
        assert_eq!(picture_cost(Quality::High, 2048, 1024, 30, 0), 422_050);
        assert_eq!(picture_cost(Quality::Low, 1024, 1024, 0, 2), 102_000);
        assert_eq!(with_margin(53_000), 66_250);
        assert_eq!(with_margin(1), FLOOR);
    }

    #[test]
    fn sound_costs_characters_or_seconds() {
        assert_eq!(speech_cost(VoiceModel::MultilingualV2, 1_000), 100_000);
        assert_eq!(speech_cost(VoiceModel::FlashV2_5, 1_000), 50_000);
        assert_eq!(music_cost(60_000), 150_000);
        assert_eq!(music_cost(10_000), 25_000);
        assert_eq!(music_cost(1), 3);
    }
}
