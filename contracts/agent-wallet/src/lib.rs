use near_sdk::near;

#[near(contract_state)]
#[derive(Default)]
pub struct AgentWallet;

#[near]
impl AgentWallet {
    pub fn contract_kind(&self) -> &'static str {
        "agent-wallet"
    }
}
