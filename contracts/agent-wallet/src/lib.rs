use near_sdk::{env, near, AccountId, Gas, NearToken, PanicOnDefault, Promise};

const LOSS: NearToken = NearToken::from_millinear(300);

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct Incident {
    policy_id: u32,
    amount: NearToken,
    recipient: AccountId,
    block_height: u64,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct AgentWallet {
    vault: AccountId,
    funded_policy_id: u32,
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
    pub fn fund_checkout(&mut self, policy_id: u32) {
        assert_eq!(env::predecessor_account_id(), self.vault, "Vault required");
        assert_eq!(env::attached_deposit(), LOSS, "Incorrect wallet funding");
        assert!(policy_id > self.funded_policy_id, "Policy already funded");
        self.funded_policy_id = policy_id;
    }

    // An explicit test fixture with a fixed destination. Deploy only with test funds.
    pub fn simulate_loss(&mut self, policy_id: u32) -> Promise {
        assert_eq!(
            env::predecessor_account_id(),
            self.runner,
            "Runner required"
        );
        assert!(self.pending.is_none(), "Transfer pending");
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
