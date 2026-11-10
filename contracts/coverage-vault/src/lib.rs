use near_sdk::{
    env, near, AccountId, Allowance, Gas, NearToken, PanicOnDefault, Promise, PromiseError,
    PromiseOrValue, PublicKey,
};

const PREMIUM: NearToken = NearToken::from_millinear(10);
const WALLET_FUNDING: NearToken = NearToken::from_millinear(300);
const LIMIT: NearToken = NearToken::from_millinear(400);
const AGENT_FUNDING: NearToken = NearToken::from_millinear(100);
const DEDUCTIBLE: NearToken = NearToken::from_millinear(50);

#[near(serializers = [borsh, json])]
pub struct Incident {
    policy_id: u32,
    amount: NearToken,
    recipient: AccountId,
    block_height: u64,
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
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
struct Policy {
    id: u32,
    activation_height: u64,
    phase: Phase,
    votes: u8,
    loss: NearToken,
    payout: NearToken,
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
    Funding,
    Refunding,
    RefundAvailable,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
struct Checkout {
    policy_id: u32,
    phase: CheckoutPhase,
}

#[near(serializers = [json])]
pub struct Dashboard {
    reserve: NearToken,
    reserved: NearToken,
    premium: NearToken,
    limit: NearToken,
    deductible: NearToken,
    policy: Option<Policy>,
    beneficiary: AccountId,
    owner: AccountId,
    agent: Option<Agent>,
    agent_funding: NearToken,
    wallet_funding: NearToken,
    checkout: Option<Checkout>,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct CoverageVault {
    owner: AccountId,
    wallet: AccountId,
    beneficiary: AccountId,
    reviewers: [AccountId; 3],
    reserve: NearToken,
    policy: Option<Policy>,
    agent: Option<Agent>,
    agent_sequence: u32,
    checkout: Option<Checkout>,
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
        Self {
            owner,
            wallet,
            beneficiary,
            reviewers,
            reserve: NearToken::from_yoctonear(0),
            policy: None,
            agent: None,
            agent_sequence: 0,
            checkout: None,
        }
    }

    pub fn contract_kind(&self) -> &'static str {
        "coverage-vault"
    }

    pub fn dashboard(&self) -> Dashboard {
        let reserved = if self.policy.as_ref().is_some_and(|p| p.phase != Phase::Paid)
            || self
                .checkout
                .as_ref()
                .is_some_and(|checkout| checkout.phase == CheckoutPhase::Funding)
        {
            LIMIT
        } else {
            NearToken::from_yoctonear(0)
        };
        Dashboard {
            reserve: self.reserve,
            reserved,
            premium: PREMIUM,
            limit: LIMIT,
            deductible: DEDUCTIBLE,
            policy: self.policy.clone(),
            beneficiary: self.beneficiary.clone(),
            owner: self.owner.clone(),
            agent: self.agent.clone(),
            agent_funding: AGENT_FUNDING,
            wallet_funding: WALLET_FUNDING,
            checkout: self.checkout.clone(),
        }
    }

    #[payable]
    pub fn fund_reserve(&mut self) {
        self.reserve = self
            .reserve
            .checked_add(env::attached_deposit())
            .expect("Reserve overflow");
    }

    #[payable]
    pub fn buy_policy(&mut self) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        assert!(self.checkout.is_none(), "Checkout already pending");
        assert!(
            self.policy.as_ref().is_none_or(|p| p.phase == Phase::Paid),
            "Policy already active"
        );
        assert_eq!(
            env::attached_deposit(),
            Self::checkout_total(),
            "Incorrect checkout amount"
        );
        assert!(self.reserve >= LIMIT, "Reserve capacity unavailable");
        let id = self
            .policy
            .as_ref()
            .map_or(1, |p| p.id.checked_add(1).expect("Policy ID overflow"));
        self.checkout = Some(Checkout {
            policy_id: id,
            phase: CheckoutPhase::Funding,
        });
        Promise::new(self.wallet.clone())
            .function_call(
                "fund_checkout",
                near_sdk::serde_json::to_vec(&near_sdk::serde_json::json!({ "policy_id": id }))
                    .expect("Policy ID"),
                WALLET_FUNDING,
                Gas::from_tgas(12),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(35))
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
            return PromiseOrValue::Promise(self.refund_checkout(false));
        }

        self.agent = None;
        self.policy = Some(Policy {
            id: checkout.policy_id,
            activation_height: env::block_height(),
            phase: Phase::Active,
            votes: 0,
            loss: NearToken::from_yoctonear(0),
            payout: NearToken::from_yoctonear(0),
        });
        self.checkout = None;
        PromiseOrValue::Value(true)
    }

    pub fn retry_checkout_refund(&mut self) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        assert!(
            self.checkout
                .as_ref()
                .is_some_and(|checkout| checkout.phase == CheckoutPhase::RefundAvailable),
            "No refund available"
        );
        self.refund_checkout(true)
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

    fn checkout_total() -> NearToken {
        PREMIUM
            .checked_add(WALLET_FUNDING)
            .expect("Checkout amount overflow")
    }

    fn refund_checkout(&mut self, retry: bool) -> Promise {
        self.checkout.as_mut().expect("Missing checkout").phase = CheckoutPhase::Refunding;
        Promise::new(self.owner.clone())
            .transfer(Self::checkout_total())
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_checkout_refund(retry),
            )
    }

    #[payable]
    pub fn connect_agent(&mut self, policy_id: u32, public_key: PublicKey) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        assert_eq!(
            env::attached_deposit(),
            AGENT_FUNDING,
            "Incorrect agent funding"
        );
        let policy = self.policy.as_ref().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Active policy required"
        );
        assert!(self.agent.is_none(), "Agent already connected or pending");
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
        Promise::new(account_id)
            .create_account()
            .transfer(AGENT_FUNDING)
            .add_access_key_allowance(
                public_key,
                Allowance::limited(AGENT_FUNDING).expect("Nonzero allowance"),
                vault.clone(),
                "submit_claim",
            )
            .then(
                Self::ext(vault)
                    .with_static_gas(Gas::from_tgas(16))
                    .finish_connect_agent(),
            )
    }

    #[private]
    pub fn finish_connect_agent(&mut self) -> bool {
        let agent = self.agent.as_mut().expect("No pending agent");
        assert!(!agent.connected, "Agent is already connected");
        if env::promise_result_checked(0, 0).is_err() {
            self.agent = None;
            Promise::new(self.owner.clone())
                .transfer(AGENT_FUNDING)
                .detach();
            return false;
        }
        agent.connected = true;
        true
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

    pub fn submit_claim(&mut self, policy_id: u32) -> Promise {
        let caller = env::predecessor_account_id();
        assert!(
            self.agent.as_ref().is_some_and(|agent| agent.connected
                && agent.account_id == caller
                && agent.public_key == env::signer_account_pk()),
            "Connected agent required"
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
            if incident.policy_id == policy.id
                && incident.block_height >= policy.activation_height
                && incident.amount > DEDUCTIBLE
            {
                policy.loss = incident.amount;
                policy.payout = incident
                    .amount
                    .checked_sub(DEDUCTIBLE)
                    .expect("Deductible")
                    .min(LIMIT);
                policy.phase = Phase::InReview;
                return true;
            }
        }
        policy.phase = Phase::Active;
        false
    }

    pub fn approve_claim(&mut self, policy_id: u32) -> u32 {
        let reviewer = self
            .reviewers
            .iter()
            .position(|r| r == &env::predecessor_account_id())
            .expect("Reviewer required");
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::InReview,
            "Claim is not in review"
        );
        let vote = 1 << reviewer;
        assert!(policy.votes & vote == 0, "Reviewer already voted");
        policy.votes |= vote;
        let count = policy.votes.count_ones();
        if count == 2 {
            policy.phase = Phase::Approved;
        }
        count
    }

    pub fn pay_claim(&mut self, policy_id: u32) -> Promise {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Approved,
            "Claim is not payable"
        );
        policy.phase = Phase::Paying;
        Promise::new(self.beneficiary.clone())
            .transfer(policy.payout)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(14))
                    .finish_payout(),
            )
    }

    #[private]
    pub fn finish_payout(&mut self) -> bool {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(policy.phase == Phase::Paying, "No pending payout");
        if env::promise_result_checked(0, 0).is_err() {
            policy.phase = Phase::Approved;
            return false;
        }
        self.reserve = self
            .reserve
            .checked_sub(policy.payout)
            .expect("Insufficient reserve");
        policy.phase = Phase::Paid;
        true
    }
}
