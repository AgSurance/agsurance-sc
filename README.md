# AgSurance contracts

Rust workspace for AgSurance, a transaction account for AI agents with coverage on NEAR.

| Package | Intended role |
|---|---|
| `agent-wallet` | Scoped agent payments, spending rules, and owner recovery |
| `coverage-vault` | Policy activation, reserved funds, and claim payouts |

Both packages currently expose a read-only contract identifier as a scaffold.

Compile both packages from the workspace:

```sh
rustup target add wasm32-unknown-unknown
cargo build --locked --release --target wasm32-unknown-unknown
```
