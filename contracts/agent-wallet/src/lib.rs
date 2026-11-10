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
        assert!(
            self.pending.is_none() && self.pending_payment.is_none(),
            "Transfer pending"
        );
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
        self.spent = self.spent.checked_add(amount).expect("Spending total");
        true
    }

    // An explicit test fixture with a fixed destination. Deploy only with test funds.
    pub fn simulate_loss(&mut self, policy_id: u32) -> Promise {
        assert_eq!(
            env::predecessor_account_id(),
            self.runner,
            "Runner required"
        );
        assert!(
            self.pending.is_none() && self.pending_payment.is_none(),
            "Transfer pending"
        );
        assert_eq!(policy_id, self.funded_policy_id, "Policy funding required");
        assert!(
            policy_id > self.incident.as_ref().map_or(0, |i| i.policy_id),
            "Loss already simulated"
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
        self.incident = Some(Incident {
            policy_id,
            amount: LOSS,
            recipient: self.recipient.clone(),
            block_height: env::block_height(),
        });
        true
    }
}
