# AgSurance contracts

Rust workspace for AgSurance, a transaction account for AI agents with coverage on NEAR.

| Package | Intended role |
|---|---|
| `agent-wallet` | Scoped agent payments, spending rules, and owner recovery |
| `coverage-vault` | Policy activation and claim review for one wallet |
| `reserve-pool` | Shared collateral, policy allocations, and claim transfers |

All three contracts run on named NEAR testnet accounts. The frontend setup deploys them without account access keys.

Compile the workspace from the workspace:

```sh
rustup target add wasm32-unknown-unknown
cargo build --locked --release --target wasm32-unknown-unknown
```

The first testnet demo uses `agent-wallet` as an explicit loss fixture. It transfers 0.3 testnet NEAR to a fixed demo recipient and saves the incident after its transfer succeeds. `coverage-vault` accepts one owner checkout for wallet funding and the premium, queries that wallet record, requires two of three reviewer approvals, and requests the capped payout from the shared reserve. Once a policy is active, the owner can grant or revoke claim submission for one dedicated agent. The vault creates the agent subaccount with an access key restricted to the vault’s `submit_claim`, `pay_supplier`, and `pay_premium` methods. The wallet enforces the configured supplier and cumulative limit. The vault retains the mandate by policy ID and requires the two reviewers to attest to the same receipt for payout. The reviewer client verifies the receipt via NEAR RPC. The agent holds its own private key. Deploy this revision to a fresh testnet vault. The vault books the coverage limit in the pool before it funds the wallet. It activates only on the successful callback. Failed funding releases the reservation and refunds the attached amount to the owner. The demo serializes one checkout and one active policy per deployment. Run the frontend's `scripts/setup-testnet.ts` to provision the contracts and the isolated test accounts.


The owner funds the operating wallet and buys the first period. A connected agent can renew from the wallet balance during the last day of the current paid week or after a missed payment. A late payment starts coverage when the vault receives it. The original policy limit stays reserved across all four weeks. The demo charges the same premium for each paid window, including late renewals. Concurrent supplier payments, premium transfers, and balance recovery are serialized. The wallet's payable `top_up` function credits funds to the operating balance.

The registered recovery account can call `pause_wallet` to stop new spending and revoke the agent. `recover_wallet` sends the wallet's tracked operating balance to the same recovery account and retires the wallet after the transfer succeeds. `refund_unused_premium` returns a premium for a prepaid window that had yet to begin at retirement. The recovery account can still file a claim for a recorded loss during a paid period before retirement. That payout comes from the shared reserve in a separate transfer.

A pool reserves 0.40 NEAR per policy and keys each allocation by vault and policy ID. A successful payout reduces assets by the transfer amount and releases that policy's allocation. A failed transfer keeps the allocation and allows retry. Pending and completed transfers are recorded in the pool so a repeated request cannot pay the same claim twice. NEAR callbacks can arrive separately from the initial call. Use the confirmed pool outcome when reconciling an uncertain payout.

The vault accepts claims until 48 hours after the term ends or the wallet retires early. Once that deadline passes, anyone can call `close_policy` to release an unused allocation. Claims still checking evidence, in review, or awaiting payout retain their allocation. Review votes bind the finalized transfer block timestamp in `block_timestamp_ms` (Unix milliseconds). Two matching votes approve a covered transfer or reject an uncovered one. The checkout and claim method names are unchanged. Use a fresh deployment for the updated contract state.

On testnet, the operator can call `quarantine_demo_collateral` to exclude a portion of collateral from coverage without moving tokens. The pool blocks new bookings and payouts while assets are below total allocations. `fund_reserve` credits new testnet collateral and restores service once all remaining allocations are backed. Claim filing, review, and the `escalate_claim` timestamp remain available. The demo keeps the existing panel as the decision-maker.
