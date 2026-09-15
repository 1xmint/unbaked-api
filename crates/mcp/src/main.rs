//! `unbaked-mcp`: an MCP stdio server. See `lib.rs`.

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    unbaked_mcp::run().await
}
