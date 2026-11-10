use near_sdk::{env, near, AccountId, Gas, NearToken, PanicOnDefault, Promise};

const CHECKOUT_FUNDING: NearToken = NearToken::from_millinear(350);
const LOSS: NearToken = NearToken::from_millinear(300);

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
pub struct Mandate {
    supplier: AccountId,
    limit: NearToken,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct AgentWallet {
    vault: AccountId,
    funded_policy_id: u32,
    mandate: Option<Mandate>,
    spent: NearToken,
    operating_balance: NearToken,
    moving_funds: bool,
    recovery_started: bool,
    retired: bool,
    pending_payment: Option<NearToken>,
    runner: AccountId,
    recipient: AccountId,
    pending: Option<u32>,
    incident: Option<Incident>,
}

#[near]
impl AgentWallet {
    #[init]
    pub fn new(vault: AccountId, runner: AccountId, recipient: AccountId) -> Self {
        assert!(env::current_account_id().as_str().ends_with(".testnet"));
        Self {
            vault,
            funded_policy_id: 0,
            mandate: None,
            spent: NearToken::from_yoctonear(0),
            operating_balance: NearToken::from_yoctonear(0),
            moving_funds: false,
            recovery_started: false,
            retired: false,
            pending_payment: None,
            runner,
            recipient,
            pending: None,
            incident: None,
        }
    }

    pub fn contract_kind(&self) -> &'static str {
        "agent-wallet"
    }

    pub fn incident(&self) -> Option<Incident> {
        self.incident.clone()
    }

    #[payable]
    pub fn fund_checkout(&mut self, policy_id: u32, mandate: Mandate) {
        assert_eq!(env::predecessor_account_id(), self.vault, "Vault required");
        assert_eq!(
            env::attached_deposit(),
            CHECKOUT_FUNDING,
            "Incorrect wallet funding"
        );
        assert!(policy_id > self.funded_policy_id, "Policy already funded");
        self.assert_idle();
        assert!(!self.recovery_started, "Wallet is in recovery");
        self.operating_balance = self
            .operating_balance
            .checked_add(CHECKOUT_FUNDING)
            .expect("Operating balance");
        self.funded_policy_id = policy_id;
        self.mandate = Some(mandate);
        self.spent = NearToken::from_yoctonear(0);
    }

    pub fn pay_supplier(
        &mut self,
        policy_id: u32,
        recipient: AccountId,
        amount: NearToken,
    ) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.vault, "Vault required");
        assert_eq!(policy_id, self.funded_policy_id, "Wrong policy");
        self.assert_idle();
        assert!(!self.recovery_started, "Wallet is in recovery");
        assert!(
            self.incident
                .as_ref()
                .is_none_or(|incident| incident.policy_id != policy_id),
            "Policy has an incident"
        );
        let mandate = self.mandate.as_ref().expect("Missing mandate");
        assert_eq!(recipient, mandate.supplier, "Recipient outside mandate");
        assert!(!amount.is_zero(), "Amount required");
        assert!(
            self.spent
                .checked_add(amount)
                .is_some_and(|total| total <= mandate.limit),
            "Spending limit exceeded"
        );
        assert!(
            self.operating_balance >= amount,
            "Insufficient operating balance"
        );
        self.pending_payment = Some(amount);
        Promise::new(recipient).transfer(amount).then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(12))
                .finish_payment(),
        )
    }

    #[private]
    pub fn finish_payment(&mut self) -> bool {
        let amount = self.pending_payment.take().expect("Missing payment");
        if env::promise_result_checked(0, 0).is_err() {
            return false;
        }
        self.debit(amount);
        self.spent = self.spent.checked_add(amount).expect("Spending total");
        true
    }

    pub fn operating_balance(&self) -> NearToken {
        self.operating_balance
    }

    #[payable]
    pub fn top_up(&mut self) {
        assert!(!self.recovery_started, "Wallet is in recovery");
        self.operating_balance = self
            .operating_balance
            .checked_add(env::attached_deposit())
            .expect("Operating balance");
    }

    pub fn send_premium(&mut self, policy_id: u32, window: u8, amount: NearToken) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.vault, "Vault required");
        assert_eq!(policy_id, self.funded_policy_id, "Wrong policy");
        self.assert_idle();
        assert!(!self.recovery_started, "Wallet is in recovery");
        assert!(
            self.operating_balance >= amount,
            "Insufficient operating balance"
        );
        self.moving_funds = true;
        Promise::new(self.vault.clone())
            .function_call(
                "accept_premium",
                near_sdk::serde_json::to_vec(
                    &near_sdk::serde_json::json!({ "policy_id": policy_id, "window": window }),
                )
                .expect("Premium parameters"),
                amount,
                Gas::from_tgas(12),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_premium(amount),
            )
    }

    #[private]
    pub fn finish_premium(&mut self, amount: NearToken) -> bool {
        self.moving_funds = false;
        if env::promise_result_checked(0, 0).is_err() {
            return false;
        }
        self.debit(amount);
        true
    }

    pub fn recover_balance(
        &mut self,
        policy_id: u32,
        beneficiary: AccountId,
    ) -> near_sdk::PromiseOrValue<bool> {
        assert_eq!(env::predecessor_account_id(), self.vault, "Vault required");
        assert_eq!(policy_id, self.funded_policy_id, "Wrong policy");
        self.assert_idle();
        assert!(!self.retired, "Wallet already retired");
        self.recovery_started = true;
        let amount = self.operating_balance;
        if amount.is_zero() {
            self.retired = true;
            return near_sdk::PromiseOrValue::Value(true);
        }
        self.moving_funds = true;
        near_sdk::PromiseOrValue::Promise(
            Promise::new(beneficiary).transfer(amount).then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_recovery(amount),
            ),
        )
    }

    #[private]
    pub fn finish_recovery(&mut self, amount: NearToken) -> bool {
        self.moving_funds = false;
        if env::promise_result_checked(0, 0).is_err() {
            return false;
        }
        self.debit(amount);
        self.retired = true;
        true
    }

    fn assert_idle(&self) {
        assert!(
            self.pending.is_none() && self.pending_payment.is_none() && !self.moving_funds,
            "Transfer pending"
        );
    }

    fn debit(&mut self, amount: NearToken) {
        self.operating_balance = self
            .operating_balance
            .checked_sub(amount)
            .expect("Operating balance");
    }

    // An explicit test fixture with a fixed destination. Deploy only with test funds.
    pub fn simulate_loss(&mut self, policy_id: u32) -> Promise {
        assert_eq!(
            env::predecessor_account_id(),
            self.runner,
            "Runner required"
        );
        self.assert_idle();
        assert!(!self.recovery_started, "Wallet is in recovery");
        assert_eq!(policy_id, self.funded_policy_id, "Policy funding required");
        assert!(
            policy_id > self.incident.as_ref().map_or(0, |i| i.policy_id),
            "Loss already simulated"
        );
        assert!(
            self.operating_balance >= LOSS,
            "Insufficient demo loss balance"
        );
        self.pending = Some(policy_id);
        Promise::new(self.recipient.clone()).transfer(LOSS).then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(12))
                .finish_loss(),
        )
    }

    #[private]
    pub fn finish_loss(&mut self) -> bool {
        let policy_id = self.pending.take().expect("Missing pending transfer");
        if env::promise_result_checked(0, 0).is_err() {
            return false;
        }
        self.debit(LOSS);
        self.incident = Some(Incident {
            policy_id,
            amount: LOSS,
            recipient: self.recipient.clone(),
            block_height: env::block_height(),
            recorded_at: env::block_timestamp_ms() / 1000,
        });
        true
    }
}
