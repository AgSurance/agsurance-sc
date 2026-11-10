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

The first testnet demo uses `agent-wallet` as an explicit loss fixture. It transfers 0.3 testnet NEAR to a fixed demo recipient and saves the incident after its transfer succeeds. `coverage-vault` sells a policy, queries that wallet record, requires two of three reviewer approvals, and transfers the capped payout to a fixed recovery account. The demo serializes one active policy per deployment. Run the frontend's `scripts/setup-testnet.ts` to provision both contracts and the isolated test accounts.
