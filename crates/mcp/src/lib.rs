//! `unbaked-mcp`: lets an agent make pictures and sounds by paying the
//! `unbaked-api` server with x402, and do Unbaked file work locally for free.

pub mod budget;
pub mod config;
mod fonts;
pub mod local;
pub mod pay;
pub mod tools;

use std::sync::Arc;

use alloy_signer_local::PrivateKeySigner;
use rmcp::ServiceExt;
use rmcp::transport::stdio;

use budget::Budget;
use config::Config;
use pay::Payer;
use tools::Server;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let config = Config::from_env()?;
    let wallet = config
        .wallet_key
        .as_ref()
        .map(|key| -> Result<_, String> {
            let signer: PrivateKeySigner = key
                .expose()
                .parse()
                .map_err(|_| "UNBAKED_WALLET_KEY is not a private key".to_owned())?;
            Ok(Arc::new(signer))
        })
        .transpose()?;
    let budget = Budget::new(config.session_cap);
    let payer = Payer::new(config.api_url.clone(), wallet, budget);
    let server = Server::new(payer, config.output_dir.clone());
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
