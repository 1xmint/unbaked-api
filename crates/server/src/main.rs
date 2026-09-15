use std::process::ExitCode;

use unbaked_api::config::Config;

#[tokio::main]
async fn main() -> ExitCode {
    // A missing .env is fine: the settings can come from the real environment.
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt().init();

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("unbaked-api: {error}");
            return ExitCode::from(2);
        }
    };
    let addr = config.addr;
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("unbaked-api: cannot listen on {addr}: {error}");
            return ExitCode::from(2);
        }
    };
    tracing::info!(%addr, network = config.network.caip2(), "listening");

    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    match axum::serve(listener, unbaked_api::app(config))
        .with_graceful_shutdown(shutdown)
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("unbaked-api: {error}");
            ExitCode::FAILURE
        }
    }
}
