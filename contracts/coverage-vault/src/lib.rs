use near_sdk::near;

#[near(contract_state)]
#[derive(Default)]
pub struct CoverageVault;

#[near]
impl CoverageVault {
    pub fn contract_kind(&self) -> &'static str {
        "coverage-vault"
    }
}
