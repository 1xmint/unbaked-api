# unbaked-api

A paid HTTP API over [Unbaked](https://github.com/1xmint/unbaked): agents make
and edit pictures and sound, paying per call with x402 (USDC). It runs on the
Base Sepolia test network only; switching to real money is refused unless it is
turned on on purpose.

Status: being built. See [docs/decisions.md](docs/decisions.md) for what was
checked and decided before the code.

## Setup

Copy `.env.example` to `.env` and fill it in. `.env` is ignored by git.

## Using it from Claude Code

`unbaked-mcp` is the agent's tool. It talks to Claude Code over MCP (standard
input and output). Pictures, speech and music are paid: the tool pays the server
from a test wallet, one call at a time. Checking, editing, previewing, listening
and rendering Unbaked files run on your own machine and are free.

1. Start the server from the repo root, so it reads `.env`:
   `cargo run -p unbaked-api`
2. Build the tool: `cargo build -p unbaked-mcp`
3. Add it to Claude Code, pointing it at the same `.env`:

   ```bash
   claude mcp add unbaked --env UNBAKED_ENV_FILE=C:/path/to/unbaked-api/.env -- C:/path/to/unbaked-api/target/debug/unbaked-mcp.exe
   ```

The tool reads these settings:

| Setting | Default | What it does |
| --- | --- | --- |
| `UNBAKED_API_URL` | `http://127.0.0.1:8402` | Where the server is. |
| `UNBAKED_WALLET_KEY` | none | The payer wallet's private key. Without it, paid tools refuse and free tools still work. It is never printed. |
| `UNBAKED_SESSION_CAP_USD` | `1` | The most one session may spend, in dollars. A call that would go over is refused before anything is signed. |
| `UNBAKED_OUTPUT_DIR` | `unbaked-output` | Where paid results are saved. `save_as` must be a plain file name. |
| `UNBAKED_ENV_FILE` | none | A `.env` file to read the settings above from. |

The tool pays only on Base Sepolia, with the `exact` scheme.
