//! The MCP tools against a real `unbaked-api` server (fake provider, fake
//! facilitator) on a real TCP listener, since the tool pays over HTTP.

use std::path::PathBuf;
use std::sync::Arc;

use alloy_signer_local::PrivateKeySigner;
use rmcp::model::ContentBlock;
use tokio::net::TcpListener;
use unbaked_api::config::Config;
use unbaked_api::providers::ProviderError;
use unbaked_api::providers::fake::{FakePictures, FakeSounds, TINY_PNG};
use unbaked_api::{Services, app_with};
use unbaked_mcp::budget::Budget;
use unbaked_mcp::pay::Payer;
use unbaked_mcp::tools::{
    AddArgs, EditArgs, EditImage, GenerateImage, Music, PathArg, PreviewArgs, RenderArgs, Server,
    Speech,
};
use unbaked_pay::fake::FakeFacilitator;

const PAY_TO: &str = "0x00000000000000000000000000000000000000bb";

struct Api {
    url: String,
    pictures: Arc<FakePictures>,
    sounds: Arc<FakeSounds>,
    payments: Arc<FakeFacilitator>,
}

async fn api() -> Api {
    let config = Config::from_lookup(|name| match name {
        "UNBAKED_API_PAY_TO" => Some(PAY_TO.to_owned()),
        _ => None,
    })
    .unwrap();
    let payments = Arc::new(FakeFacilitator::new());
    let pictures = Arc::new(FakePictures::new());
    let sounds = Arc::new(FakeSounds::new());
    let services = Services {
        facilitator: Some(payments.clone()),
        pictures: Some(pictures.clone()),
        sounds: Some(sounds.clone()),
    };
    let app = app_with(config, services);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Api {
        url: format!("http://{addr}"),
        pictures,
        sounds,
        payments,
    }
}

fn server(api: &Api, cap_usd: &str, wallet: bool) -> Server {
    let wallet = wallet.then(|| Arc::new(PrivateKeySigner::random()));
    let cap = unbaked_mcp::config::micro_dollars(cap_usd).unwrap();
    let payer = Payer::new(api.url.clone(), wallet, Budget::new(cap));
    Server::new(
        payer,
        std::env::temp_dir().join(format!("unbaked-mcp-test-{}", uid())),
    )
}

fn uid() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

fn text_of(result: &rmcp::model::CallToolResult) -> String {
    result
        .content
        .iter()
        .find_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn is_error(result: &rmcp::model::CallToolResult) -> bool {
    result.is_error.unwrap_or(false)
}

#[tokio::test]
async fn generate_image_pays_once_and_saves_the_file() {
    let api = api().await;
    let server = server(&api, "1.00", true);
    let result = server
        .generate_image(rmcp::handler::server::wrapper::Parameters(GenerateImage {
            prompt: "a pear on a table".into(),
            size: None,
            quality: None,
            format: None,
            background: None,
            save_as: Some("pear.png".into()),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    let report: serde_json::Value = serde_json::from_str(&text_of(&result)).unwrap();
    let saved = std::fs::read(report["output"].as_str().unwrap()).unwrap();
    assert_eq!(saved, TINY_PNG);
    assert_eq!(api.payments.settles(), 1);
    assert!(report["paid_usd"].as_str().unwrap().parse::<f64>().unwrap() > 0.0);
}

#[tokio::test]
async fn a_price_over_the_session_cap_is_refused_before_signing() {
    let api = api().await;
    let server = server(&api, "0.0001", true);
    let result = server
        .generate_image(rmcp::handler::server::wrapper::Parameters(GenerateImage {
            prompt: "a pear on a table".into(),
            size: None,
            quality: None,
            format: None,
            background: None,
            save_as: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&result));
    assert!(
        text_of(&result).contains("UNBAKED_SESSION_CAP_USD"),
        "{}",
        text_of(&result)
    );
    assert_eq!(api.payments.verifies(), 0);
}

#[tokio::test]
async fn a_provider_failure_releases_the_reservation() {
    let api = api().await;
    api.pictures
        .fail_with(ProviderError::Refused("blocked".into()));
    let server = server(&api, "1.00", true);
    let result = server
        .generate_image(rmcp::handler::server::wrapper::Parameters(GenerateImage {
            prompt: "a pear on a table".into(),
            size: None,
            quality: None,
            format: None,
            background: None,
            save_as: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&result));
    assert_eq!(api.payments.settles(), 0);
    assert_eq!(
        server.payer().budget().remaining(),
        server.payer().budget().cap()
    );
}

#[tokio::test]
async fn speech_and_music_save_mp3s() {
    let api = api().await;
    let server = server(&api, "1.00", true);

    let result = server
        .speech(rmcp::handler::server::wrapper::Parameters(Speech {
            text: "hello".into(),
            voice_id: "v1".into(),
            model: None,
            language_code: None,
            seed: None,
            save_as: Some("hello.mp3".into()),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));

    let result = server
        .music(rmcp::handler::server::wrapper::Parameters(Music {
            prompt: "a jingle".into(),
            length_ms: 4_000,
            instrumental: Some(true),
            seed: None,
            save_as: Some("jingle.mp3".into()),
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    assert_eq!(api.sounds.speeches().len(), 1);
    assert_eq!(api.sounds.songs().len(), 1);
}

#[tokio::test]
async fn edit_image_sends_two_images_and_a_mask() {
    let api = api().await;
    let server = server(&api, "1.00", true);
    let dir = std::env::temp_dir().join(format!("unbaked-mcp-src-{}", uid()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.png");
    let b = dir.join("b.png");
    let mask = dir.join("mask.png");
    std::fs::write(&a, TINY_PNG).unwrap();
    std::fs::write(&b, TINY_PNG).unwrap();
    std::fs::write(&mask, TINY_PNG).unwrap();

    let result = server
        .edit_image(rmcp::handler::server::wrapper::Parameters(EditImage {
            prompt: "make it night".into(),
            images: vec![a.display().to_string(), b.display().to_string()],
            mask: Some(mask.display().to_string()),
            size: None,
            quality: None,
            format: None,
            background: None,
            save_as: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&result), "{}", text_of(&result));
    let edits = api.pictures.edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].images.len(), 2);
    assert!(edits[0].mask.is_some());
}

#[tokio::test]
async fn no_wallet_key_refuses_paid_tools_but_frees_still_work() {
    let api = api().await;
    let server = server(&api, "1.00", false);
    let result = server
        .generate_image(rmcp::handler::server::wrapper::Parameters(GenerateImage {
            prompt: "a pear".into(),
            size: None,
            quality: None,
            format: None,
            background: None,
            save_as: None,
        }))
        .await
        .unwrap();
    assert!(is_error(&result));
    assert!(text_of(&result).contains("UNBAKED_WALLET_KEY"));

    let voices = server.voices().await.unwrap();
    assert!(!is_error(&voices));
}

#[tokio::test]
async fn save_as_with_a_separator_is_refused() {
    let api = api().await;
    let server = server(&api, "1.00", true);
    let result = server
        .generate_image(rmcp::handler::server::wrapper::Parameters(GenerateImage {
            prompt: "a pear".into(),
            size: None,
            quality: None,
            format: None,
            background: None,
            save_as: Some("../evil.png".into()),
        }))
        .await
        .unwrap();
    assert!(is_error(&result));
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../server/tests/fixtures")
        .join(name)
}

#[tokio::test]
async fn free_local_tools_work_on_the_fixture() {
    let api = api().await;
    let server = server(&api, "1.00", true);
    let dir = std::env::temp_dir().join(format!("unbaked-mcp-file-{}", uid()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tiny.unbaked.png");
    std::fs::copy(fixture("tiny.unbaked.png"), &path).unwrap();
    let path = path.display().to_string();

    let check = server
        .check(rmcp::handler::server::wrapper::Parameters(PathArg {
            path: path.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&check), "{}", text_of(&check));

    let estimate = server
        .estimate(rmcp::handler::server::wrapper::Parameters(PathArg {
            path: path.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_error(&estimate), "{}", text_of(&estimate));

    let preview = server
        .preview(rmcp::handler::server::wrapper::Parameters(PreviewArgs {
            path: path.clone(),
            output: Some("preview.png".into()),
            at_ms: None,
            max_edge: None,
            sheet: None,
            fonts: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&preview), "{}", text_of(&preview));
    assert!(
        preview
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image(_)))
    );

    let listen = server
        .listen(rmcp::handler::server::wrapper::Parameters(PathArg {
            path: path.clone(),
        }))
        .await;
    let _ = listen; // an image fixture has no sound track; errors are fine here

    let edit = server
        .edit(rmcp::handler::server::wrapper::Parameters(EditArgs {
            path: path.clone(),
            patch: serde_json::json!([{"op": "add", "path": "/output/background", "value": "#ffffffff"}]),
            output: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&edit), "{}", text_of(&edit));

    let add = server
        .add(rmcp::handler::server::wrapper::Parameters(AddArgs {
            path: path.clone(),
            media: {
                let media = dir.join("asset.png");
                std::fs::write(&media, TINY_PNG).unwrap();
                media.display().to_string()
            },
            id: "extra".into(),
            license: None,
            output: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&add), "{}", text_of(&add));

    let render = server
        .render(rmcp::handler::server::wrapper::Parameters(RenderArgs {
            path: path.clone(),
            output: None,
            fonts: None,
        }))
        .await
        .unwrap();
    assert!(!is_error(&render), "{}", text_of(&render));

    let check_again = server
        .check(rmcp::handler::server::wrapper::Parameters(PathArg { path }))
        .await
        .unwrap();
    let report: serde_json::Value = serde_json::from_str(&text_of(&check_again)).unwrap();
    assert_eq!(report["status"], "fresh");
}
