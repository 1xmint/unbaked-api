# Decisions (step 0 checks, 2026-09-14)

What was checked before any code, what it found, and what it decided. Every
fact below was read from its source on 2026-09-14; the source is named next to
it. Anything marked "not checked" is a guess until a later PR proves it.

## Payments (x402 v2)

**The Rust crates are community-made.** `x402-axum` 2.0.2, `x402-reqwest`
2.0.2, `x402-chain-eip155` 2.0.2 and `x402-types` all come from
github.com/x402-rs/x402-rs (Apache-2.0, last commit 2026-07-13). The x402
Foundation publishes no Rust crate. A crate named plain `x402` is an abandoned
squat; never use it.

**`x402-axum` settles in the right order but cannot price from the body.** By
default it verifies, runs the handler, skips settling on any 4xx or 5xx, then
settles (`crates/x402-axum/src/paygate.rs`, the after-execution branch). Its
price hook, `PriceTagSource::resolve(headers, uri, base_url)`, never sees the
request body. Our prices depend on the body (picture size, text length, render
work), and our guards (one job per payment, refusing a payer whose settlement
failed, a daily spend cap) need hooks it does not have.

*Decision:* the payment gate in `crates/pay` is our own, written in PR 2. It
keeps Radar's order and fail-closed rules, uses async `reqwest`, and reads the
body before pricing. PR 2 decides whether to borrow the wire types from
`x402-types` or write the few structs by hand, by whichever builds lighter.

**`x402-reqwest` fits the agent's side as it is.** It sends `Payment-Signature`
for v2 (`crates/x402-reqwest/src/client.rs`), signs with an EVM private key
(`alloy_signer_local::PrivateKeySigner`), knows Base Sepolia, and has a
`MaxAmount` selector that refuses to pay above a set amount per request
(`x402-types/src/scheme/client.rs`). The MCP tool uses it in PR 6.

**Facilitator: the free public one for testing, Coinbase's later.** The
facilitator is the service that checks a payment and moves the money.

- `https://x402.org/facilitator` needs no key. Its `/supported` answered 200
  with `{"x402Version":2,"scheme":"exact","network":"eip155:84532"}`.
- Coinbase's CDP facilitator (`POST /v2/x402/verify` and `/settle` on
  `api.cdp.coinbase.com`) needs a CDP API key even on the test network: a
  request without one got `401 Unauthorized`. Its docs: the first 1,000
  settlements a month are free, then $0.001 each; checking is always free.
- x402-rs has no helper for Coinbase's signed login token; we would write it.

*Decision:* PR 2 builds one facilitator client that takes a base URL and
optional login headers. Testing uses x402.org, so no Coinbase key is needed
now. Coinbase's key and token signing come with the move to real money.

**Base Sepolia facts** (Circle's USDC address page, and
`x402-chain-eip155/src/networks.rs`, which agree):

| Fact | Value |
|---|---|
| Network id | `eip155:84532` |
| USDC contract | `0x036CbD53842c5426634e7929541eC2318f3dCF7e` |
| USDC decimals | 6 |
| Signing domain (EIP-712) | name `USDC`, version `2` |
| Test USDC | faucet.circle.com, or portal.cdp.coinbase.com/products/faucet |

The payer signs a transfer permission and the facilitator pays the gas, so the
payer wallet needs test USDC only.

## MCP tool

`rmcp` 3.3.0 (2026-09-10) is current. Default features include `server` and
`macros`; stdio needs `transport-io` added. A tools-only server is a struct
with `#[tool_router(server_handler)]` on its impl and `#[tool(description =
"...")]` on each method, started with `.serve(stdio())` (rmcp 3.3.0 README).

## OpenAI pictures

- Model `gpt-image-2`, pinned snapshot `gpt-image-2-2026-04-21`
  (developers.openai.com model page). Newer `gpt-image-2.5` variants exist
  (snapshot 2026-09-08); we stay pinned and revisit after the first test.
- Generate: `POST /v1/images/generations`. Edit: `POST /v1/images/edits`,
  multipart with `image[]` and an optional `mask` (a PNG under 4 MB, same size
  as the source; see-through areas mark what to change).
- Sizes: 1024×1024, 1536×1024, 1024×1536, or custom (multiples of 16, sides
  between 1:3 and 3:1, longest side at most 3840, 655,360 to 8,294,400 pixels).
  Quality: `low`, `medium`, `high`, `auto`. `output_format`: png, webp, jpeg.
  GPT picture models return base64 only.
- Price is by token: $8 per million input tokens, $30 per million output
  tokens. A 1024×1024 picture works out near $0.006 low, $0.053 medium, $0.211
  high (a third-party calculation that matches the token prices).
- **OpenAI requires identity verification** ("API Organization Verification",
  government ID, up to 24 hours) before GPT picture models answer.
- *Not checked:* the exact response field name (`b64_json` expected). The live
  test in PR 4 settles it, and also reads the reported token counts to check
  the price table.

## ElevenLabs sound

- Auth header `xi-api-key`.
- Speech: `POST /v1/text-to-speech/{voice_id}`, `output_format`
  `mp3_44100_128` and 27 others. Models and per-request character limits:
  `eleven_v3` 5,000; `eleven_multilingual_v2` 10,000; `eleven_flash_v2_5`
  40,000. Price per 1,000 characters: $0.10 (v3, multilingual v2), $0.05
  (Flash, Turbo).
- Music: compose length 3 s to 10 min, $0.15 per minute. *Not checked:* which
  paid plan first allows music through the API.
- Sound effects: `POST /v1/sound-generation`, 0.5 to 30 s, $0.12 per minute.
- The free plan is for non-commercial use only (terms of use, section 1(c)).
  Any paid plan allows commercial use.

**Their rules forbid reselling sound effects as files.** The Prohibited Use
Policy (updated 17 August 2026), section 9(c), bans selling Sound Effects
output "on a standalone basis for any purpose, including as isolated files".
A per-call sound effect endpoint is exactly that.

*Decision:* no `/v1/sound-effect` endpoint and no `sound_effect` tool. The
first test (voice-over over music) does not need one.

**Two more clauses need ElevenLabs' written answer before real money.**
Section 9(b) bars reselling their services without written authorization, and
9(i) bars "applications or software that interact with our Services without
our prior written authorization (such as through our APIs)". Read literally,
9(i) covers this whole API. This joins the launch gates; it does not block the
test on this PC, where the only buyer is Josh.

## What Josh sets up (Claude cannot make accounts or handle keys)

All values go in a `.env` file at the repo root, which git ignores. Copy
`.env.example` and fill it in.

1. **OpenAI API key**, after finishing identity verification in the OpenAI
   developer console.
2. **ElevenLabs API key**, on a paid plan (Starter is the cheapest). This
   costs real money each month, so it is Josh's call; the free plan would
   work for speech on this PC but may not allow music.
3. **A receiving address on Base Sepolia**: any EVM wallet address. Only the
   address goes in `.env`, never that wallet's key.
4. **A separate payer test wallet** holding a few test USDC from
   faucet.circle.com (pick Base Sepolia). Its private key goes in `.env` as
   `UNBAKED_WALLET_KEY`. Use a wallet made only for this and never put real
   money in it.

No Coinbase key is needed until the move to real money.

## PR 3: file endpoints (2026-09-14)

- **Renders run in the server process**, on a blocking thread with render limits and a deadline, not as an `unbaked` child process. Layer 1 checks its deadline between rows, layers, frames and sound chunks, and the server's limits (36 million pixels per buffer, 10 minutes of stereo) bound memory. This saves shipping and locating a second binary. Revisit before hosting strangers' jobs: a child process can be killed outright.
- **Prices live in code** (`crates/server/src/prices.rs`), not a `prices.toml`. File work costs 1 millionth of a USDC per estimated millisecond at 0.66 ms per work unit, with a floor of $0.005. 0.66 is the only bench rate on record (an image); sound and video rates must be measured before real money.
- **Fonts:** the server has no font folder, so recipes that reference fonts by SHA-256 fail with `font_not_found`; packed fonts work.
- **Base (real money) terms** use USDC `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913` with EIP-712 name "USD Coin" version 2, from memory. Check against the contract before real money; a wrong name only makes payments fail verification.
