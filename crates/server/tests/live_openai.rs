//! One real call to OpenAI, run by hand; it costs about a cent:
//!
//! cargo test -p unbaked-api --test live_openai -- --ignored --nocapture
//!
//! Needs OPENAI_API_KEY in `.env` at the repo root (or the environment).

use unbaked_api::openai::OpenAi;
use unbaked_api::prices::picture_cost;
use unbaked_api::providers::{Background, PictureFormat, PictureJob, Pictures, Quality};

#[tokio::test]
#[ignore = "calls OpenAI and costs money"]
async fn openai_makes_a_low_quality_png() {
    let _ = dotenvy::from_filename("../../.env");
    let key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY is not set");
    let openai = OpenAi::new(&key).unwrap();
    let job = PictureJob {
        prompt: "a ripe pear on a plain blue table, soft light".to_owned(),
        width: 1024,
        height: 1024,
        quality: Quality::Low,
        format: PictureFormat::Png,
        background: Background::Auto,
    };
    let estimate = picture_cost(job.quality, job.width, job.height, job.prompt.len(), 0);
    let picture = openai.generate(job).await.unwrap();
    assert!(PictureFormat::Png.matches(&picture.bytes));

    let usage = picture.usage.expect("OpenAI reported no token counts");
    // Text in $5, output $30, per million tokens.
    let actual = usage.text_tokens * 5 + usage.output_tokens * 30;
    println!(
        "{} bytes; tokens: text {} image {} output {}; cost ${:.6}, table says ${:.6}",
        picture.bytes.len(),
        usage.text_tokens,
        usage.image_tokens,
        usage.output_tokens,
        actual as f64 / 1e6,
        estimate as f64 / 1e6,
    );
    std::fs::write(
        std::env::temp_dir().join("unbaked-live-openai.png"),
        &picture.bytes,
    )
    .unwrap();
}
