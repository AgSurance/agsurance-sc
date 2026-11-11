mod claim_recovery;

use claim_recovery::{ClaimRecovery, RecoveryRecipient};
use near_sdk::store::{LookupMap, LookupSet};
use near_sdk::{
    env, near, AccountId, Allowance, Gas, NearToken, PanicOnDefault, Promise, PromiseError,
    PromiseOrValue, PublicKey,
};

mod limit_increase;
mod observed_claim;
use limit_increase::{LimitChange, LimitIncrease};
mod premium;
mod recovery_account;
use observed_claim::ObservedClaim;

const REVIEW_SECONDS: u32 = 72 * 60 * 60;
const CLAIM_SECONDS: u32 = 48 * 60 * 60;
const WEEK: u32 = 7 * 24 * 60 * 60;
const WINDOW_COUNT: usize = 4;

const ZERO: NearToken = NearToken::from_yoctonear(0);
const PREMIUM: NearToken = NearToken::from_millinear(10);
const WALLET_FUNDING: NearToken = NearToken::from_millinear(350);
const MAX_SUPPLIER_LIMIT: NearToken = NearToken::from_millinear(50);
const LIMIT: NearToken = NearToken::from_millinear(400);
const AGENT_FUNDING: NearToken = NearToken::from_millinear(100);
const DEDUCTIBLE: NearToken = NearToken::from_millinear(50);

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct Incident {
    policy_id: u32,
    amount: NearToken,
    recipient: AccountId,
    block_height: u64,
    recorded_at: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Active,
    CheckingEvidence,
    InReview,
    Approved,
    Paying,
    Paid,
    Rejected,
    Closing,
    Closed,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct Mandate {
    supplier: AccountId,
    limit: NearToken,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, PartialEq, Eq)]
pub struct Evidence {
    transaction_hash: String,
    receipt_id: String,
    block_height: u64,
    recipient: AccountId,
    amount: NearToken,
    block_timestamp_ms: Option<u64>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
struct Policy {
    id: u32,
    activation_height: u64,
    coverage_limit: NearToken,
    limit_changes: Vec<LimitChange>,
    paid_premiums: [NearToken; WINDOW_COUNT],
    claim_limit: NearToken,
    phase: Phase,
    votes: u8,
    loss: NearToken,
    payout: NearToken,
    mandate: Mandate,
    incident: Option<Incident>,
    evidence: Option<Evidence>,
    observed_claim: Option<ObservedClaim>,
    windows: [Option<u64>; WINDOW_COUNT],
    review_deadline: Option<u64>,
    escalated_at: Option<u64>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
struct Agent {
    account_id: AccountId,
    public_key: PublicKey,
    connected: bool,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckoutPhase {
    CheckingBalance,
    Booking,
    Funding,
    Releasing,
    ReleaseAvailable,
    Refunding,
    RefundAvailable,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
struct Checkout {
    policy_id: u32,
    phase: CheckoutPhase,
    mandate: Mandate,
    coverage_limit: NearToken,
    wallet_funding: NearToken,
    premium: NearToken,
}

#[near(serializers = [json])]
struct Coverage {
    active: bool,
    current_window: Option<u8>,
    payable_window: Option<u8>,
    next_boundary: Option<u64>,
    renewal_pending: bool,
    paused: bool,
    recovery_pending: bool,
    migration_started: bool,
    retired: bool,
    unused_premium: bool,
    filing_deadline: Option<u64>,
    term_end: Option<u64>,
    retired_at_ms: Option<u64>,
    window_seconds: u32,
}

#[near(serializers = [json])]
#[derive(PartialEq, Eq)]
pub enum Payment {
    Pending,
    Retry,
    Paid,
}

#[near(serializers = [json])]
pub struct Dashboard {
    reserve_pool: AccountId,
    reserved: NearToken,
    premium: NearToken,
    limit: NearToken,
    deductible: NearToken,
    policy: Option<Policy>,
    claim_recovery: Option<ClaimRecovery>,
    beneficiary: AccountId,
    owner: AccountId,
    agent: Option<Agent>,
    agent_funding: NearToken,
    wallet_funding: NearToken,
    max_supplier_limit: NearToken,
    checkout: Option<Checkout>,
    coverage: Coverage,
    chain_time: u64,
    wallet: AccountId,
    wallet_code_hash: Option<String>,
    recovery_choice_available: bool,
    pending_beneficiary: Option<AccountId>,
    rolling: bool,
    limit_increase: Option<LimitIncrease>,
    premiums_distributed: bool,
    premiums_pending: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct CoverageVault {
    owner: AccountId,
    wallet: AccountId,
    beneficiary: AccountId,
    reviewers: [AccountId; 3],
    reserve_pool: AccountId,
    policy: Option<Policy>,
    agent: Option<Agent>,
    agent_sequence: u32,
    agent_keys: LookupSet<PublicKey>,
    mandates: LookupMap<u32, Mandate>,
    checkout: Option<Checkout>,
    window_seconds: u32,
    review_seconds: u32,
    renewal: Option<u8>,
    paused: bool,
    recovery_pending: bool,
    migration_started: bool,
    retired_at_ms: Option<u64>,
    premium_refund_pending: bool,
    claim_seconds: u32,
    policy_sequence: u32,
    capital_pilot: bool,
    premiums_pending: bool,
    premiums_distributed: bool,
    claim_recoveries: LookupMap<u32, ClaimRecovery>,
    payout_attempt_pending: bool,
    wallet_code_hash: Option<String>,
    used_transfer_actions: LookupSet<(String, u32)>,
    vendor_confirmations: LookupMap<u32, observed_claim::VendorConfirmation>,
    beneficiary_chosen: bool,
    increase_sequence: u32,
    pending_beneficiary: Option<AccountId>,
    rolling: bool,
    limit_increase: Option<LimitIncrease>,
}

#[near]
impl CoverageVault {
    #[init]
    pub fn new(
        owner: AccountId,
        wallet: AccountId,
        runner: AccountId,
        beneficiary: AccountId,
        reviewers: [AccountId; 3],
        test_window_seconds: Option<u32>,
        reserve_pool: AccountId,
        test_review_seconds: Option<u32>,
        test_claim_seconds: Option<u32>,
        capital_pilot: Option<bool>,
        wallet_code_hash: Option<String>,
        rolling: Option<bool>,
    ) -> Self {
        assert!(env::current_account_id().as_str().ends_with(".testnet"));
        assert!(
            reviewers[0] != reviewers[1]
                && reviewers[0] != reviewers[2]
                && reviewers[1] != reviewers[2],
            "Reviewers must be distinct"
        );
        assert!(
            reviewers
                .iter()
                .all(|r| r != &owner && r != &runner && r != &wallet && r != &beneficiary),
            "Reviewers must have separate roles"
        );
        let window_seconds = test_window_seconds.unwrap_or(WEEK);
        assert!(
            (30..=WEEK).contains(&window_seconds),
            "Invalid window length"
        );
        assert!(
            beneficiary != wallet && beneficiary != env::current_account_id(),
            "Invalid recovery account"
        );
        let review_seconds = test_review_seconds.unwrap_or(REVIEW_SECONDS);
        let claim_seconds = test_claim_seconds.unwrap_or(CLAIM_SECONDS);
        assert!(
            (1..=REVIEW_SECONDS).contains(&review_seconds),
            "Invalid review deadline"
        );
        assert!(
            (1..=CLAIM_SECONDS).contains(&claim_seconds),
            "Invalid filing window"
        );
        assert!(
            reserve_pool.as_str().ends_with(".testnet")
                && reserve_pool != env::current_account_id(),
            "Invalid reserve pool"
        );
        Self {
            owner,
            wallet,
            beneficiary,
            reviewers,
            reserve_pool,
            policy: None,
            agent: None,
            agent_sequence: 0,
            agent_keys: LookupSet::new(b"agent-keys".to_vec()),
            mandates: LookupMap::new(b"m"),
            checkout: None,
            window_seconds,
            review_seconds,
            renewal: None,
            paused: false,
            recovery_pending: false,
            migration_started: false,
            retired_at_ms: None,
            premium_refund_pending: false,
            claim_seconds,
            policy_sequence: 0,
            capital_pilot: capital_pilot.unwrap_or(false),
            premiums_pending: false,
            premiums_distributed: false,
            claim_recoveries: LookupMap::new(b"c"),
            payout_attempt_pending: false,
            wallet_code_hash,
            used_transfer_actions: LookupSet::new(b"t"),
            vendor_confirmations: LookupMap::new(b"v"),
            pending_beneficiary: None,
            beneficiary_chosen: false,
            rolling: rolling.unwrap_or(false),
            limit_increase: None,
            increase_sequence: 0,
        }
    }

    pub fn contract_kind(&self) -> &'static str {
        "coverage-vault"
    }

    pub fn dashboard(&self) -> Dashboard {
        let limit = self
            .checkout
            .as_ref()
            .map(|checkout| checkout.coverage_limit)
            .or_else(|| self.policy.as_ref().map(Policy::current_limit))
            .unwrap_or(LIMIT);
        let reserved = if self
            .policy
            .as_ref()
            .is_some_and(|p| !matches!(p.phase, Phase::Paid | Phase::Closed))
            || self.checkout.as_ref().is_some_and(|checkout| {
                matches!(
                    checkout.phase,
                    CheckoutPhase::Funding
                        | CheckoutPhase::Releasing
                        | CheckoutPhase::ReleaseAvailable
                )
            }) {
            limit
        } else {
            NearToken::from_yoctonear(0)
        };
        Dashboard {
            reserve_pool: self.reserve_pool.clone(),
            reserved,
            premium: self
                .checkout
                .as_ref()
                .map_or(self.premium_for(limit), |checkout| checkout.premium),
            limit,
            deductible: DEDUCTIBLE,
            policy: self.policy.clone(),
            claim_recovery: self
                .policy
                .as_ref()
                .and_then(|policy| self.claim_recovery(policy.id)),
            beneficiary: self.beneficiary.clone(),
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            agent_funding: AGENT_FUNDING,
            wallet_funding: self
                .checkout
                .as_ref()
                .map_or(WALLET_FUNDING, |checkout| checkout.wallet_funding),
            max_supplier_limit: MAX_SUPPLIER_LIMIT,
            checkout: self.checkout.clone(),
            coverage: self.coverage(),
            chain_time: Self::now(),
            wallet: self.wallet.clone(),
            wallet_code_hash: self.wallet_code_hash.clone(),
            recovery_choice_available: !self.beneficiary_chosen
                && self.policy_sequence == 0
                && self.checkout.is_none(),
            pending_beneficiary: self.pending_beneficiary.clone(),
            rolling: self.rolling,
            limit_increase: self.limit_increase.clone(),
            premiums_distributed: self.premiums_distributed,
            premiums_pending: self.premiums_pending,
        }
    }

    #[payable]
    pub fn buy_policy(
        &mut self,
        supplier: AccountId,
        limit: NearToken,
        expected_policy_id: Option<u32>,
        coverage_limit: Option<NearToken>,
        wallet_funding: Option<NearToken>,
    ) -> Promise {
        assert!(
            supplier.as_str().ends_with(".testnet")
                && supplier != self.wallet
                && supplier != env::current_account_id(),
            "Invalid supplier"
        );
        assert!(
            !limit.is_zero() && limit <= MAX_SUPPLIER_LIMIT,
            "Invalid spending limit"
        );
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        if let Some(id) = expected_policy_id {
            assert_eq!(
                id,
                self.policy.as_ref().map_or(0, |policy| policy.id),
                "Stale policy"
            );
        }
        assert!(self.checkout.is_none(), "Checkout already pending");
        assert!(
            self.pending_beneficiary.is_none(),
            "Recovery account confirmation pending"
        );
        assert!(
            !self.capital_pilot || self.policy.is_none() || self.premiums_distributed,
            "Settle earned premiums first"
        );
        assert!(!self.paused && self.renewal.is_none(), "Wallet unavailable");
        assert!(
            self.agent.as_ref().is_none_or(|agent| agent.connected),
            "Agent connection pending"
        );
        assert!(
            self.policy
                .as_ref()
                .is_none_or(|p| matches!(p.phase, Phase::Paid | Phase::Closed)),
            "Policy already active"
        );
        let (coverage_limit, wallet_funding) = if self.rolling {
            let coverage_limit = coverage_limit.expect("Choose a coverage limit");
            assert!(
                coverage_limit >= NearToken::from_millinear(1),
                "Minimum limit is 0.001 NEAR"
            );
            (
                coverage_limit,
                wallet_funding.expect("Specify wallet funding"),
            )
        } else {
            assert!(
                coverage_limit.is_none() && wallet_funding.is_none(),
                "Fixed checkout required"
            );
            (LIMIT, WALLET_FUNDING)
        };
        let premium = self.premium_for(coverage_limit);
        assert_eq!(
            env::attached_deposit(),
            premium
                .checked_add(wallet_funding)
                .expect("Checkout overflow"),
            "Incorrect checkout amount"
        );
        self.policy_sequence = self
            .policy_sequence
            .checked_add(1)
            .expect("Policy ID overflow");
        let id = self.policy_sequence;
        let mandate = Mandate { supplier, limit };
        self.checkout = Some(Checkout {
            policy_id: id,
            phase: if self.rolling {
                CheckoutPhase::CheckingBalance
            } else {
                CheckoutPhase::Booking
            },
            mandate,
            coverage_limit,
            wallet_funding,
            premium,
        });
        if self.rolling {
            return self.wallet_balance().then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(125))
                    .finish_checkout_balance(),
            );
        }
        self.book_checkout()
    }

    #[private]
    pub fn finish_checkout_balance(
        &mut self,
        #[callback_result] balance: Result<NearToken, PromiseError>,
    ) -> Promise {
        let checkout = self.checkout.as_mut().expect("Missing checkout");
        assert!(
            checkout.phase == CheckoutPhase::CheckingBalance,
            "No checkout balance check"
        );
        if balance.is_err()
            || checkout.coverage_limit
                > balance
                    .unwrap()
                    .saturating_add(checkout.wallet_funding)
                    .saturating_sub(DEDUCTIBLE)
        {
            return self.refund_checkout(false);
        }
        checkout.phase = CheckoutPhase::Booking;
        self.book_checkout()
    }

    fn book_checkout(&self) -> Promise {
        let checkout = self.checkout.as_ref().expect("Missing checkout");
        let args = if self.rolling {
            near_sdk::serde_json::json!({"policy_id": checkout.policy_id, "limit": checkout.coverage_limit})
        } else {
            near_sdk::serde_json::json!({"policy_id": checkout.policy_id})
        };
        self.pool_call("reserve_policy", args, 16).then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(83))
                .finish_booking(),
        )
    }

    #[private]
    pub fn finish_booking(
        &mut self,
        #[callback_result] result: Result<bool, PromiseError>,
    ) -> Promise {
        let checkout = self.checkout.as_mut().expect("Missing checkout");
        assert!(
            checkout.phase == CheckoutPhase::Booking,
            "Checkout is not booking"
        );
        match result {
            Ok(true) => checkout.phase = CheckoutPhase::Funding,
            Ok(false) => return self.refund_checkout(false),
            Err(_) => return self.release_checkout(false),
        }
        let id = checkout.policy_id;
        let mandate = &checkout.mandate;
        let args = if self.rolling {
            near_sdk::serde_json::json!({"policy_id": id, "mandate": mandate, "funding": checkout.wallet_funding})
        } else {
            near_sdk::serde_json::json!({"policy_id": id, "mandate": mandate})
        };
        Promise::new(self.wallet.clone())
            .function_call(
                "fund_checkout",
                near_sdk::serde_json::to_vec(&args).expect("Policy ID"),
                checkout.wallet_funding,
                Gas::from_tgas(12),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(55))
                    .finish_checkout(),
            )
    }

    #[private]
    pub fn finish_checkout(&mut self) -> PromiseOrValue<bool> {
        let checkout = self.checkout.as_ref().expect("Missing checkout");
        assert!(
            checkout.phase == CheckoutPhase::Funding,
            "Checkout is not funding"
        );
        if env::promise_result_checked(0, 0).is_err() {
            return PromiseOrValue::Promise(self.release_checkout(false));
        }

        self.agent = None;
        self.mandates
            .insert(checkout.policy_id, checkout.mandate.clone());
        self.policy = Some(Policy {
            id: checkout.policy_id,
            activation_height: env::block_height(),
            coverage_limit: checkout.coverage_limit,
            limit_changes: Vec::new(),
            paid_premiums: [checkout.premium, ZERO, ZERO, ZERO],
            claim_limit: checkout.coverage_limit,
            phase: Phase::Active,
            votes: 0,
            loss: NearToken::from_yoctonear(0),
            payout: NearToken::from_yoctonear(0),
            mandate: checkout.mandate.clone(),
            incident: None,
            evidence: None,
            observed_claim: None,
            windows: [Some(Self::now()), None, None, None],
            review_deadline: None,
            escalated_at: None,
        });
        self.checkout = None;
        self.premiums_distributed = false;
        PromiseOrValue::Value(true)
    }

    pub fn retry_checkout_refund(&mut self, policy_id: Option<u32>) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        let checkout = self.checkout.as_ref().expect("No refund available");
        assert!(
            policy_id.is_none_or(|id| id == checkout.policy_id),
            "Stale checkout"
        );
        match checkout.phase {
            CheckoutPhase::RefundAvailable => self.refund_checkout(true),
            CheckoutPhase::ReleaseAvailable => self.release_checkout(true),
            _ => env::panic_str("No refund available"),
        }
    }

    pub fn close_policy(&mut self, policy_id: u32) -> Promise {
        self.assert_no_increase();
        let deadline = self.filing_deadline().expect("No policy");
        let policy = self.policy.as_mut().expect("No policy");
        assert_eq!(policy.id, policy_id, "Wrong policy");
        assert!(Self::now() >= deadline, "Claim filing window is open");
        assert!(
            matches!(
                policy.phase,
                Phase::Active | Phase::Rejected | Phase::Closing
            ),
            "Policy cannot be closed"
        );
        assert!(
            self.checkout.is_none() && self.renewal.is_none(),
            "Payment pending"
        );
        policy.phase = Phase::Closing;
        self.pool_call(
            "release_closed_policy",
            near_sdk::serde_json::json!({ "policy_id": policy_id }),
            12,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(14))
                .finish_close(policy_id),
        )
    }

    #[private]
    pub fn finish_close(&mut self, policy_id: u32) -> bool {
        let policy = self.policy.as_mut().expect("No policy");
        if policy.id != policy_id {
            return false;
        }
        if policy.phase == Phase::Closed {
            return true;
        }
        if policy.phase != Phase::Closing || env::promise_result_checked(0, 0).is_err() {
            return false;
        }
        policy.phase = Phase::Closed;
        true
    }

    fn release_checkout(&mut self, retry: bool) -> Promise {
        let checkout = self.checkout.as_mut().expect("Missing checkout");
        checkout.phase = CheckoutPhase::Releasing;
        let id = checkout.policy_id;
        self.pool_call(
            "release_failed_checkout",
            near_sdk::serde_json::json!({ "policy_id": id }),
            12,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(30))
                .finish_release(retry),
        )
    }

    #[private]
    pub fn finish_release(&mut self, retry: bool) -> PromiseOrValue<bool> {
        let checkout = self.checkout.as_mut().expect("Missing checkout");
        assert!(
            checkout.phase == CheckoutPhase::Releasing,
            "Checkout is not releasing"
        );
        if env::promise_result_checked(0, 0).is_err() {
            checkout.phase = CheckoutPhase::ReleaseAvailable;
            return PromiseOrValue::Value(false);
        }
        PromiseOrValue::Promise(self.refund_checkout(retry))
    }

    fn pool_call(&self, method: &str, args: near_sdk::serde_json::Value, gas: u64) -> Promise {
        Promise::new(self.reserve_pool.clone()).function_call(
            method,
            near_sdk::serde_json::to_vec(&args).expect("Pool parameters"),
            NearToken::from_yoctonear(0),
            Gas::from_tgas(gas),
        )
    }

    #[private]
    pub fn finish_checkout_refund(&mut self, retry: bool) -> bool {
        let checkout = self.checkout.as_mut().expect("Missing checkout refund");
        assert!(
            checkout.phase == CheckoutPhase::Refunding,
            "Checkout is not refunding"
        );
        if env::promise_result_checked(0, 0).is_ok() {
            self.checkout = None;
            return retry;
        }
        checkout.phase = CheckoutPhase::RefundAvailable;
        false
    }

    fn premium_for(&self, limit: NearToken) -> NearToken {
        if self.rolling {
            NearToken::from_yoctonear(limit.as_yoctonear().div_ceil(40))
        } else {
            PREMIUM
        }
    }

    fn refund_checkout(&mut self, retry: bool) -> Promise {
        let checkout = self.checkout.as_mut().expect("Missing checkout");
        checkout.phase = CheckoutPhase::Refunding;
        Promise::new(self.owner.clone())
            .transfer(
                checkout
                    .premium
                    .checked_add(checkout.wallet_funding)
                    .expect("Checkout overflow"),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_checkout_refund(retry),
            )
    }

    #[payable]
    pub fn connect_agent(&mut self, policy_id: u32, public_key: PublicKey) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        let policy = self.policy.as_ref().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Active policy required"
        );
        assert!(!self.paused, "Wallet paused");
        assert!(self.checkout.is_none(), "Checkout pending");
        self.register_agent(public_key)
    }

    #[payable]
    pub fn rotate_agent(&mut self, policy_id: u32, public_key: PublicKey) -> Promise {
        self.assert_recovery_account(policy_id);
        self.assert_can_resume();
        self.register_agent(public_key)
    }

    fn register_agent(&mut self, public_key: PublicKey) -> Promise {
        assert_eq!(
            env::attached_deposit(),
            AGENT_FUNDING,
            "Incorrect agent funding"
        );
        assert!(self.agent.is_none(), "Agent already connected or pending");
        assert!(
            self.agent_keys.insert(public_key.clone()),
            "Fresh agent key required"
        );
        let funder = env::predecessor_account_id();
        self.agent_sequence = self
            .agent_sequence
            .checked_add(1)
            .expect("Agent ID overflow");
        let vault = env::current_account_id();
        let account_id: AccountId = format!("agent-{}.{}", self.agent_sequence, vault)
            .parse()
            .expect("Vault account ID is too long for an agent subaccount");
        self.agent = Some(Agent {
            account_id: account_id.clone(),
            public_key: public_key.clone(),
            connected: false,
        });
        Promise::new(account_id.clone())
            .create_account()
            .transfer(AGENT_FUNDING)
            .add_access_key_allowance(
                public_key.clone(),
                Allowance::limited(AGENT_FUNDING).expect("Nonzero allowance"),
                vault.clone(),
                "submit_claim,submit_evidence_claim,pay_supplier,pay_premium",
            )
            .then(
                Self::ext(vault)
                    .with_static_gas(Gas::from_tgas(16))
                    .finish_connect_agent(account_id, public_key, funder),
            )
    }

    #[private]
    pub fn finish_connect_agent(
        &mut self,
        account_id: AccountId,
        public_key: PublicKey,
        funder: AccountId,
    ) -> bool {
        let created = env::promise_result_checked(0, 0).is_ok();
        if !created {
            self.agent_keys.remove(&public_key);
            Promise::new(funder).transfer(AGENT_FUNDING).detach();
        }
        // Pause can revoke an in-flight registration before account creation completes.
        let Some(agent) = self
            .agent
            .as_mut()
            .filter(|agent| agent.account_id == account_id)
        else {
            return false;
        };
        if created {
            agent.connected = true;
        } else {
            self.agent = None;
        }
        created
    }

    #[payable]
    pub fn revoke_agent(&mut self, policy_id: u32) {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        assert_eq!(
            env::attached_deposit(),
            NearToken::from_yoctonear(1),
            "Full access approval required"
        );
        assert_eq!(
            self.policy.as_ref().map(|policy| policy.id),
            Some(policy_id),
            "Wrong policy"
        );
        assert!(
            self.agent.as_ref().is_some_and(|agent| agent.connected),
            "No connected agent"
        );
        self.agent = None;
    }

    pub fn mandate(&self, policy_id: u32) -> Option<Mandate> {
        self.mandates.get(&policy_id).cloned()
    }

    pub fn pay_supplier(&self, policy_id: u32, recipient: AccountId, amount: NearToken) -> Promise {
        self.assert_connected_agent();
        assert!(!self.paused, "Wallet paused");
        assert!(self.coverage().active, "Coverage is inactive");
        let policy = self.policy.as_ref().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Active policy required"
        );
        Promise::new(self.wallet.clone()).function_call(
            "pay_supplier",
            near_sdk::serde_json::to_vec(&near_sdk::serde_json::json!({
                "policy_id": policy_id, "recipient": recipient, "amount": amount,
            }))
            .expect("Payment parameters"),
            NearToken::from_yoctonear(0),
            Gas::from_tgas(40),
        )
    }

    pub fn pay_premium(&mut self, policy_id: u32) -> Promise {
        self.assert_connected_agent();
        assert!(!self.paused, "Wallet paused");
        assert!(self.renewal.is_none(), "Premium already pending");
        let policy = self.policy.as_ref().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Active policy required"
        );
        self.assert_no_increase();
        let premium = self.premium_for(policy.current_limit());
        let window = self.coverage().payable_window.expect("No premium due");
        self.renewal = Some(window);
        Promise::new(self.wallet.clone())
            .function_call(
                "send_premium",
                near_sdk::serde_json::to_vec(&near_sdk::serde_json::json!({
                    "policy_id": policy_id, "window": window, "amount": premium,
                }))
                .expect("Premium parameters"),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(44),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_renewal(),
            )
    }

    #[payable]
    pub fn accept_premium(&mut self, policy_id: u32, window: u8) {
        assert_eq!(
            env::predecessor_account_id(),
            self.wallet,
            "Wallet required"
        );
        let premium = self.premium_for(self.policy.as_ref().expect("No policy").current_limit());
        assert_eq!(env::attached_deposit(), premium, "Incorrect premium");
        assert_eq!(self.renewal, Some(window), "No matching pending premium");
        assert!(!self.paused, "Wallet paused");
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Active policy required"
        );
        let index = usize::from(window);
        let start = Self::window_start(policy, self.window_seconds, index);
        let now = Self::now();
        assert!(
            now < start + u64::from(self.window_seconds),
            "Window has ended"
        );
        assert!(policy.windows[index].is_none(), "Premium already paid");
        policy.windows[index] = Some(start.max(now));
        policy.paid_premiums[index] = premium;
    }

    #[private]
    pub fn finish_renewal(
        &mut self,
        #[callback_result] result: Result<bool, PromiseError>,
    ) -> bool {
        let window = self.renewal.take().expect("No pending premium");
        result.unwrap_or(false)
            && self
                .policy
                .as_ref()
                .is_some_and(|policy| policy.windows[usize::from(window)].is_some())
    }

    pub fn pause_wallet(&mut self, policy_id: u32) {
        self.assert_no_increase();
        self.assert_recovery_account(policy_id);
        self.paused = true;
        self.agent = None;
    }

    pub fn resume_wallet(&mut self, policy_id: u32) {
        self.assert_no_increase();
        self.assert_recovery_account(policy_id);
        self.assert_can_resume();
        assert!(self.renewal.is_none(), "Premium pending");
        assert!(
            self.agent.as_ref().is_some_and(|agent| agent.connected),
            "Replacement agent not connected"
        );
        self.paused = false;
    }

    fn assert_recovery_account(&self, policy_id: u32) {
        assert_eq!(
            env::predecessor_account_id(),
            self.beneficiary,
            "Recovery account required"
        );
        assert_eq!(
            self.policy.as_ref().map(|policy| policy.id),
            Some(policy_id),
            "Wrong policy"
        );
        assert!(self.checkout.is_none(), "Checkout pending");
    }

    fn assert_can_resume(&self) {
        assert!(
            self.paused && !self.migration_started && self.retired_at_ms.is_none(),
            "Wallet cannot be resumed"
        );
    }

    pub fn recover_wallet(&mut self, policy_id: u32) -> Promise {
        self.assert_no_increase();
        self.assert_recovery_account(policy_id);
        assert!(
            self.paused && !self.recovery_pending && self.retired_at_ms.is_none(),
            "Wallet cannot be recovered"
        );
        assert!(self.renewal.is_none(), "Premium pending");
        self.migration_started = true;
        self.recovery_pending = true;
        self.agent = None;
        Promise::new(self.wallet.clone())
            .function_call(
                "recover_balance",
                near_sdk::serde_json::to_vec(&near_sdk::serde_json::json!({
                    "policy_id": policy_id, "beneficiary": self.beneficiary,
                }))
                .expect("Recovery parameters"),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(40),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_recovery(),
            )
    }

    #[private]
    pub fn finish_recovery(
        &mut self,
        #[callback_result] result: Result<bool, PromiseError>,
    ) -> bool {
        self.recovery_pending = false;
        if result.unwrap_or(false) {
            self.retired_at_ms = Some(env::block_timestamp_ms());
            return true;
        }
        false
    }

    pub fn refund_unused_premium(&mut self, policy_id: u32) -> Promise {
        assert_eq!(
            env::predecessor_account_id(),
            self.beneficiary,
            "Recovery account required"
        );
        assert!(!self.premium_refund_pending, "Refund pending");
        let policy = self.policy.as_ref().expect("No policy");
        assert_eq!(policy.id, policy_id, "Wrong policy");
        let window = self.unused_window().expect("No refundable premium");
        self.premium_refund_pending = true;
        Promise::new(self.beneficiary.clone())
            .transfer(policy.paid_premiums[window])
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_premium_refund(window as u8),
            )
    }

    #[private]
    pub fn finish_premium_refund(&mut self, window: u8) -> bool {
        self.premium_refund_pending = false;
        if env::promise_result_checked(0, 0).is_err() {
            return false;
        }
        let policy = self.policy.as_mut().expect("No policy");
        policy.windows[usize::from(window)] = None;
        policy.paid_premiums[usize::from(window)] = ZERO;
        true
    }

    fn now() -> u64 {
        env::block_timestamp_ms() / 1000
    }

    fn window_at(policy: &Policy, seconds: u32, time: u64) -> Option<usize> {
        let index = time.checked_sub(Self::window_start(policy, seconds, 0))? / u64::from(seconds);
        (index < WINDOW_COUNT as u64).then_some(index as usize)
    }

    fn window_start(policy: &Policy, seconds: u32, index: usize) -> u64 {
        policy.windows[0].expect("First window") + index as u64 * u64::from(seconds)
    }

    fn unused_window(&self) -> Option<usize> {
        let retired_at_ms = self.retired_at_ms?;
        self.policy
            .as_ref()?
            .windows
            .iter()
            .position(|start| start.is_some_and(|start| start * 1000 > retired_at_ms))
    }

    fn term_end(&self, policy: &Policy) -> u64 {
        let scheduled_end = Self::window_start(policy, self.window_seconds, WINDOW_COUNT);
        self.retired_at_ms
            .map_or(scheduled_end, |retired| (retired / 1000).min(scheduled_end))
    }

    fn filing_deadline(&self) -> Option<u64> {
        let policy = self.policy.as_ref()?;
        Some(self.term_end(policy) + u64::from(self.claim_seconds))
    }

    fn coverage(&self) -> Coverage {
        let now = Self::now();
        let mut current_window = None;
        let mut payable_window = None;
        let mut next_boundary = None;
        let mut active = false;
        if let Some(policy) = &self.policy {
            if let Some(index) = Self::window_at(policy, self.window_seconds, now) {
                current_window = Some(index as u8);
                let boundary = Self::window_start(policy, self.window_seconds, index + 1);
                next_boundary = Some(boundary);
                active = self.retired_at_ms.is_none()
                    && policy.windows[index].is_some_and(|start| start <= now);
                if !self.paused && policy.phase == Phase::Active {
                    if !active {
                        payable_window = Some(index as u8);
                    } else if boundary - now <= u64::from(self.window_seconds / 7)
                        && index + 1 < WINDOW_COUNT
                        && policy.windows[index + 1].is_none()
                    {
                        payable_window = Some((index + 1) as u8);
                    }
                }
            }
        }
        Coverage {
            active,
            current_window,
            payable_window,
            next_boundary,
            renewal_pending: self.renewal.is_some(),
            paused: self.paused,
            recovery_pending: self.recovery_pending,
            migration_started: self.migration_started,
            retired: self.retired_at_ms.is_some(),
            unused_premium: !self.premium_refund_pending && self.unused_window().is_some(),
            filing_deadline: self.filing_deadline(),
            term_end: self.policy.as_ref().map(|policy| self.term_end(policy)),
            retired_at_ms: self.retired_at_ms,
            window_seconds: self.window_seconds,
        }
    }

    fn assert_connected_agent(&self) {
        let caller = env::predecessor_account_id();
        assert!(
            self.agent.as_ref().is_some_and(|agent| agent.connected
                && agent.account_id == caller
                && agent.public_key == env::signer_account_pk()),
            "Connected agent required"
        );
    }

    pub fn submit_claim(&mut self, policy_id: u32) -> Promise {
        self.assert_no_increase();
        let caller = env::predecessor_account_id();
        if caller != self.beneficiary && caller != self.owner {
            self.assert_connected_agent();
        }
        assert!(
            Self::now() < self.filing_deadline().expect("No policy"),
            "Claim filing window has closed"
        );
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Policy cannot accept claim"
        );
        policy.phase = Phase::CheckingEvidence;
        Promise::new(self.wallet.clone())
            .function_call(
                "incident",
                b"{}".to_vec(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(8),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(15))
                    .verify_incident(),
            )
    }

    #[private]
    pub fn verify_incident(
        &mut self,
        #[callback_result] result: Result<Option<Incident>, PromiseError>,
    ) -> bool {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.phase == Phase::CheckingEvidence,
            "No pending evidence"
        );
        if let Ok(Some(incident)) = result {
            if incident.policy_id == policy.id && incident.amount > DEDUCTIBLE {
                policy.loss = incident.amount;
                policy.claim_limit = policy.limit_at(incident.block_height);
                policy.payout = incident
                    .amount
                    .checked_sub(DEDUCTIBLE)
                    .expect("Deductible")
                    .min(policy.claim_limit);
                policy.incident = Some(incident);
                policy.phase = Phase::InReview;
                policy.review_deadline = Some(Self::now() + u64::from(self.review_seconds));
                return true;
            }
        }
        policy.phase = Phase::Active;
        false
    }

    pub fn approve_claim(&mut self, policy_id: u32, evidence: Evidence) -> u32 {
        self.vote_claim(policy_id, evidence, true)
    }

    pub fn reject_claim(&mut self, policy_id: u32, evidence: Evidence) -> u32 {
        self.vote_claim(policy_id, evidence, false)
    }

    fn vote_claim(&mut self, policy_id: u32, evidence: Evidence, approve: bool) -> u32 {
        let reviewer = self
            .reviewers
            .iter()
            .position(|r| r == &env::predecessor_account_id())
            .expect("Reviewer required");
        let policy = self.policy.as_ref().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::InReview,
            "Claim is not in review"
        );
        let incident = policy.incident.as_ref().expect("Missing claim incident");
        assert!(
            evidence.recipient == incident.recipient
                && evidence.amount == incident.amount
                && evidence.block_height <= incident.block_height,
            "Evidence does not match the recorded loss"
        );
        assert!(
            evidence.recipient != policy.mandate.supplier || evidence.amount > policy.mandate.limit,
            "Transfer matches the mandate"
        );
        let time_ms = evidence
            .block_timestamp_ms
            .expect("Transfer block timestamp required");
        let time = time_ms / 1000;
        assert!(time <= incident.recorded_at, "Transfer follows incident");
        let covered = self.transfer_is_covered(policy, &evidence);
        assert_eq!(
            approve, covered,
            "Vote does not match coverage at transfer time"
        );
        let recovery = self
            .claim_recoveries
            .get(&policy_id)
            .cloned()
            .unwrap_or_default();
        let policy = self.policy.as_mut().expect("No policy");
        if let Some(previous) = &policy.evidence {
            assert!(
                previous == &evidence,
                "Reviewers must attest to the same receipt"
            );
        } else {
            policy.claim_limit = policy.limit_at(evidence.block_height);
            policy.payout = recovery.payout(policy.loss, policy.claim_limit);
            policy.evidence = Some(evidence);
        }
        let vote = 1 << reviewer;
        assert!(policy.votes & vote == 0, "Reviewer already voted");
        policy.votes |= vote;
        let count = policy.votes.count_ones();
        if count == 2 {
            policy.phase = if approve {
                let receipt = policy
                    .evidence
                    .as_ref()
                    .expect("Matching evidence")
                    .receipt_id
                    .clone();
                assert!(
                    self.used_transfer_actions.insert((receipt, 0)),
                    "Transfer already claimed"
                );
                Phase::Approved
            } else {
                Phase::Rejected
            };
        }
        count
    }

    pub fn escalate_claim(&mut self, policy_id: u32) {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::InReview,
            "Claim is not in review"
        );
        let now = Self::now();
        assert!(
            policy
                .review_deadline
                .is_some_and(|deadline| now >= deadline),
            "Review deadline has not passed"
        );
        assert!(policy.escalated_at.is_none(), "Claim already escalated");
        policy.escalated_at = Some(now);
    }

    pub fn claim_recovery(&self, policy_id: u32) -> Option<ClaimRecovery> {
        self.claim_recoveries.get(&policy_id).cloned().or_else(|| {
            self.policy
                .as_ref()
                .filter(|policy| policy.id == policy_id)
                .map(|_| ClaimRecovery::default())
        })
    }

    // Record only the NEAR actually returned to this claim after external recovery costs.
    // Remaining wallet balance is not incident recovery.
    #[payable]
    pub fn record_claim_recovery(&mut self, policy_id: u32) -> ClaimRecovery {
        assert_eq!(
            env::predecessor_account_id(),
            self.beneficiary,
            "Recovery account required"
        );
        let mut recovery = self
            .claim_recoveries
            .get(&policy_id)
            .cloned()
            .unwrap_or_default();
        if recovery.is_settled() {
            recovery.record(env::attached_deposit(), None);
        } else {
            let policy = self.policy.as_mut().expect("No policy");
            assert!(
                policy.id == policy_id && matches!(policy.phase, Phase::InReview | Phase::Approved),
                "Claim must be in review, approved, or paid"
            );
            recovery.record(env::attached_deposit(), Some(policy.loss));
            if let Some(claim) = &policy.observed_claim {
                policy.claim_limit =
                    policy.observed_claim_limit(&claim.transfers, recovery.recovered_before_payout);
            }
            policy.payout = recovery.payout(policy.loss, policy.claim_limit);
        }
        self.claim_recoveries.insert(policy_id, recovery.clone());
        recovery
    }

    pub fn distribute_claim_recovery(
        &mut self,
        policy_id: u32,
        recipient: RecoveryRecipient,
    ) -> Promise {
        let recovery = self
            .claim_recoveries
            .get_mut(&policy_id)
            .expect("No claim recovery");
        let amount = recovery.begin_distribution(recipient);
        let distribution =
            match recipient {
                RecoveryRecipient::Beneficiary => {
                    Promise::new(self.beneficiary.clone()).transfer(amount)
                }
                RecoveryRecipient::Reserve => Promise::new(self.reserve_pool.clone())
                    .function_call("fund_reserve", b"{}".to_vec(), amount, Gas::from_tgas(15)),
            };
        distribution.then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(18))
                .finish_recovery_distribution(policy_id, recipient),
        )
    }

    #[private]
    pub fn finish_recovery_distribution(
        &mut self,
        policy_id: u32,
        recipient: RecoveryRecipient,
    ) -> bool {
        let success = env::promise_result_checked(0, 0).is_ok();
        self.claim_recoveries
            .get_mut(&policy_id)
            .expect("No claim recovery")
            .finish_distribution(recipient, success);
        success
    }

    pub fn pay_claim(&mut self, policy_id: u32) -> Promise {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && matches!(policy.phase, Phase::Approved | Phase::Paying),
            "Claim is not payable"
        );
        assert!(!self.payout_attempt_pending, "Payout attempt pending");
        self.payout_attempt_pending = true;
        policy.phase = Phase::Paying;
        let amount = policy.payout;
        self.pool_call(
            "settle_claim",
            near_sdk::serde_json::json!({ "policy_id": policy_id, "amount": amount }),
            40,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(22))
                .finish_payout(policy_id),
        )
    }

    #[private]
    pub fn finish_payout(
        &mut self,
        policy_id: u32,
        #[callback_result] result: Result<Payment, PromiseError>,
    ) -> bool {
        let policy = self.policy.as_mut().expect("No policy");
        if policy.id != policy_id {
            return false;
        }
        if policy.phase != Phase::Paying || !self.payout_attempt_pending {
            return policy.phase == Phase::Paid;
        }
        self.payout_attempt_pending = false;
        match result {
            Ok(Payment::Paid) => {
                let mut recovery = self
                    .claim_recoveries
                    .get(&policy_id)
                    .cloned()
                    .unwrap_or_default();
                recovery.settle(policy.loss, policy.payout);
                self.claim_recoveries.insert(policy_id, recovery);
                policy.phase = Phase::Paid;
                true
            }
            Ok(Payment::Retry) => {
                policy.phase = Phase::Approved;
                false
            }
            Ok(Payment::Pending) | Err(_) => false,
        }
    }
}
