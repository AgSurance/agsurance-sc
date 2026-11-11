# AgSurance contracts

AgSurance runs on NEAR testnet. The wallet holds the agent's spending funds, the vault manages coverage and claims, and the reserve holds the capital for approved payouts. The public demo uses a factory to deploy a wallet and vault for each buyer.

[Try the live shared-reserve demo](https://agsurance-judge-preview.vercel.app/demo#manual-demo) · [0.18 NEAR payout](https://testnet.nearblocks.io/txns/7bNezR6hT22Bm2MCeSPc59BC2ehBaYSExTQdqn98MRYP/receipts) · [Run the local demo](https://github.com/AgSurance/agsurance-fe#run-the-testnet-demo) · [Open the public replay](https://agsurance.github.io/agsurance-fe/)

A second capital deposit opened capacity for a previously blocked checkout. The first policy kept its 0.18 NEAR limit until the owner raised it to 0.42 NEAR. The reserve paid 0.18 NEAR for a covered loss that occurred before the increase. [Capital deposit](https://testnet.nearblocks.io/txns/81QPfbkG2uhDAKc5HL4NhUCqMJijAd1wkHqdfoGQQ49U/receipts) · [Limit increase](https://testnet.nearblocks.io/txns/CL3ko61db2p4Mw2tZaLEaj9hPWLaXZpW3tipt7CyrkNV/receipts) · [0.18 NEAR payout](https://testnet.nearblocks.io/txns/7bNezR6hT22Bm2MCeSPc59BC2ehBaYSExTQdqn98MRYP/receipts)

## System diagram

The owner uses the local dashboard to buy a policy. The agent signs calls from its own runtime. The reviewer client reads transfer results through NEAR RPC. The vault counts the votes that reviewer accounts submit to the contract.

```mermaid
flowchart LR
    Dashboard[Local dashboard] -->|Owner and recovery actions| Vault
    Agent[Agent CLI] -->|Restricted function calls| Vault
    Reviewer[Reviewer client] -->|Reads receipts| RPC[NEAR RPC]
    Reviewer -->|Submits votes| Vault

    subgraph Testnet[NEAR testnet]
        Vault[coverage-vault] -->|Funds wallet and requests supplier payments| Wallet[agent-wallet]
        Wallet -->|Returns recorded loss| Vault
        Vault -->|Reserves capacity and requests payout| Pool[reserve-pool]
        Wallet -->|Pays| Supplier[Approved supplier]
        Wallet -->|Returns remaining wallet funds| Recovery[Recovery account]
        Pool -->|Pays approved claim| Recovery
    end

    Replay[Public replay] -->|Links to transaction receipts| Explorer[NearBlocks]
```

| Contract | What it controls |
| --- | --- |
| [`agent-wallet`](contracts/agent-wallet/src/lib.rs) | The spending balance, approved supplier, spending limit, and balance recovery. |
| [`coverage-vault`](contracts/coverage-vault/src/lib.rs) | Policy purchase, paid periods, agent access, and claim review. |
| [`reserve-pool`](contracts/reserve-pool/src/lib.rs) | Capital deposits, policy allocations, exposure caps, and claim payouts. |
| [`manual-demo-factory`](contracts/manual-demo-factory/src/lib.rs) | Provisions buyer wallets and vaults and registers them with the reserve. |

## Open a fixed-limit replay policy and grant access

A callback is a method that receives the result of an earlier call. The vault uses callbacks because NEAR processes calls between contracts in separate steps.

1. The owner attaches 0.36 NEAR to `buy_policy` and provides the supplier and spending limit. The vault asks the pool to set aside 0.40 NEAR through `reserve_policy`.
2. If the pool sets aside the funds, `finish_booking` calls `fund_checkout` to send 0.35 NEAR to the wallet. The vault keeps the 0.01 NEAR premium.
3. If wallet funding succeeds, `finish_checkout` activates coverage. If funding fails, the vault releases the allocation and refunds the owner.
4. The owner calls `connect_agent` with the public key and 0.10 NEAR. The vault creates an agent subaccount with a function-call access key.

The key permits `pay_supplier`, `pay_premium`, and `submit_claim` on the vault. The wallet enforces the supplier and the total spending limit for the policy. The vault checks the agent's access when it receives a request. The owner can call `revoke_agent` to remove that access.

## Review a claim and pay it

The test function `simulate_loss` transfers 0.30 NEAR to a fixed test recipient. The wallet records the loss after the transfer succeeds. When the agent calls `submit_claim`, the vault reads the recorded loss from the wallet.

The reviewer client reads the finalized transfer receipt through NEAR RPC. The vault compares the submitted evidence with the recorded loss and the policy's spending rules. It uses the transfer time to determine whether coverage was active. Two of three reviewers must submit matching votes on the same receipt. The contract uses those votes to approve or reject the claim.

New vaults initialized with `wallet_code_hash` also accept `submit_evidence_claim` from the owner, recovery account, or a permitted agent. It groups up to eight distinct transfer actions by receipt and action index, validates them against the policy's time windows and mandate, and applies one deductible. Each `vote_evidence_claim` is bound to the same evidence hash. Reviewers independently check the finalized receipts and the historical wallet code through NEAR RPC.

After approval, `pay_claim` calls `settle_claim` on the pool. The pool pays the registered recovery account. `finish_transfer` records the transfer result. The vault marks the claim paid when `finish_payout` receives a paid result. If the transfer fails, the pool keeps the allocation for a retry. The pool records pending and paid transfers to prevent a second payout for the same policy. The standard 0.30 NEAR test loss pays 0.25 NEAR after the 0.05 NEAR deductible.

The registered recovery account calls payable `record_claim_recovery` to attach NEAR recovered from the incident after any external recovery costs. A deposit of 0.05 NEAR before payout reduces the test payout to 0.20 NEAR. A zero payout settles the claim and releases its allocation.

For deposits after payout, the vault first credits the recovery account for loss above the limit, then credits the reserve up to the payout, then credits the recovery account for the deductible. `distribute_claim_recovery` sends each credit to its fixed account and preserves the credit if the transfer fails. `claim_recovery` returns recorded totals by policy ID.

## Renew, recover, and release funds

The owner pays for the first coverage period during checkout. The agent can renew during the last day of a paid period or after a missed payment. A late payment starts coverage when the vault receives it. Each renewal costs 0.01 NEAR. The original policy limit applies across all four periods. The wallet's payable `top_up` method adds funds to the operating balance.

The registered recovery account calls `pause_wallet` to stop new spending and revoke the old agent grant. Scheduled transfers settle independently. To keep the wallet, register a fresh public key through `rotate_agent` with 0.10 NEAR. Call `resume_wallet` after the replacement grant is confirmed and any pending premium has settled.

Before checkout, the owner can register a separate beneficiary with `choose_recovery_account`. The reserve confirms and locks that payout destination. The supplier can use `confirm_vendor_change` to record an expiring confirmation of a new address for use in a future checkout. The current mandate remains in force for the term.

To retire the wallet, call `recover_wallet` to move the operating balance to the registered recovery account. Starting the transfer disables resumption. The account can retry a failed transfer, file a claim for a covered loss before retirement, and use `refund_unused_premium` to recover a prepaid period that had yet to begin.

The filing window closes 48 hours after the policy ends or the wallet retires. After that deadline, anyone can call `close_policy` to release unused funds. An open claim keeps its allocation. If the reviewers do not reach two matching votes by the default 72-hour review deadline, `escalate_claim` records the escalation for the same panel.

A pool can serve several registered vaults. Each vault manages one wallet and one active policy. If the pool has less than it owes across all allocations, it blocks new allocations and payouts. Claim filing and review stay available. Testnet operators can use `quarantine_demo_collateral` to simulate a shortfall and `fund_reserve` to restore collateral.

## Optional capital pilot

Capital providers fund a cohort that the operator seals before issuing policies. The pool limits delegation to confirmed liquid assets left after policy allocations and the configured buffer. It restores staking principal to liquid assets after the returned funds reach the reserve and credits realized staking rewards separately.

After a policy ends and its claim or reserve allocation has settled, the vault can call `settle_earned_premiums`. The standard pilot configuration credits 20% cashback to the owner of a fully paid term without a coverage gap, then assigns 70% of the remaining earned premium to the cohort and the balance to the operator.

Providers claim their proportional principal and earnings after the cohort closes. The pool also tracks their shares of paid claims. The recovery, premium, and cashback flows passed in NEAR Sandbox. The sandbox tested the full staking flow with a simulated validator. A separate live testnet pool delegated 0.05 NEAR and reached unbonding. Withdrawal and rewards are pending. [Pilot setup and operation](https://github.com/AgSurance/agsurance-fe#optional-capital-pilot)

## Build the contracts

Install the Rust toolchain. Run these commands from the `agsurance-sc` root.

```sh
rustup target add wasm32-unknown-unknown
cargo build --locked --release --target wasm32-unknown-unknown
```

The build writes the WebAssembly files to `target/wasm32-unknown-unknown/release`. Use a fresh deployment for this contract state. The frontend setup script deploys the three contracts to named testnet accounts without account access keys. Follow the [frontend setup instructions](https://github.com/AgSurance/agsurance-fe#run-the-testnet-demo).

## Manual demo factory

Build the wallet and vault with the standard command above, then embed those binaries in the factory.

```sh
wasm_dir="$PWD/target/wasm32-unknown-unknown/release"
AGSURANCE_WALLET_WASM="$wasm_dir/agent_wallet.wasm" \
AGSURANCE_VAULT_WASM="$wasm_dir/coverage_vault.wasm" \
cargo build --locked --release --target wasm32-unknown-unknown -p manual-demo-factory
```

Deploy the factory to a named testnet account with at most 24 characters. Use a **fresh** reserve and factory. Keep the reserve operator account separate from the factory registrar. Initialize the reserve with `new({ operator })`, then have the operator call `configure_capital_pilot` and `configure_rolling_limits({ max_policy, max_group, registrar })`. Set `registrar` to the factory account. All amounts are yoctoNEAR. Initialize the factory with the reserve, the wallet code hash, three distinct reviewer accounts, a runner, a test incident recipient, and a `risk_group` shared by vaults that use the same wallet runtime. Seed the reserve via `fund_capital_pilot` to receive capital shares. The first deposit enables issuance when capacity is sufficient.

The reserve locks the quoted payout limit and keeps the configured buffer liquid. It enforces both underwriting caps on purchases and increases. The vault charges 2.5% of the limit per weekly testnet window, rounded up to the next yoctoNEAR, with a 0.05 NEAR deductible and a 0.001 NEAR minimum limit. An increase charges the fee difference for the current window and any already-paid later windows. Claims use the limit in effect at the transfer block.

Later deposits buy shares at current pool equity (liquid assets plus booked provider earnings when the validator is idle). They share all future payouts, including claims on policies already active. Previously paid losses are reflected in the entry price. Deposits pause while payouts or validator funds are in flight; only surplus beyond existing allocations and the buffer may be delegated. Legacy deployments retain their fixed pilot and checkout terms. Use fresh testnet accounts for the shared reserve; the new WASM uses a different storage layout.

A buyer calls `setup_deposit` for the storage amount and signs `prepare` with at least 255 Tgas. The factory creates the wallet and vault without full-access keys and registers the buyer as the recovery account. When `setup` reports `Ready`, the buyer signs `buy_policy` on the new vault. If setup stops after a failed receipt, call `prepare` again without another deposit.

## What to test

Use a fresh testnet setup. The [frontend instructions](https://github.com/AgSurance/agsurance-fe#live-regression-checks) include the command for live regression tests. These tests send transactions on NEAR testnet.

| Action | Expected result |
| --- | --- |
| Request payment to a supplier outside the policy. | The wallet rejects the request. The supplier receives no funds. |
| Pause the wallet, register a replacement key, and resume. | The old key loses access. The replacement can pay the supplier from the same wallet. |
| Approve a test claim with two matching votes. | The vault accepts a payout request. The pool pays the registered recovery account. |
| Record 0.05 NEAR recovered before settling a 0.30 NEAR loss. | The pool pays 0.20 NEAR, and the recovery account can collect the 0.05 NEAR deposit separately. |
| Call `pay_claim` again after the claim reaches `Paid`. | The vault rejects the request. The pool sends no second transfer. |
