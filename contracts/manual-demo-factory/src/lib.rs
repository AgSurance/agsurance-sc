use near_sdk::serde_json::{json, Value};
use near_sdk::store::LookupMap;
use near_sdk::{
    env, near, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PromiseOrValue,
};

const WALLET_CODE: &[u8] = include_bytes!(env!("AGSURANCE_WALLET_WASM"));
const VAULT_CODE: &[u8] = include_bytes!(env!("AGSURANCE_VAULT_WASM"));
const NO_DEPOSIT: NearToken = NearToken::from_yoctonear(0);
const REGISTRY_FEE: NearToken = NearToken::from_millinear(50);

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy)]
enum Stage {
    Wallet,
    Vault,
    Register,
    Ready,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct Setup {
    wallet: AccountId,
    vault: AccountId,
    stage: Stage,
    pending: bool,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct ManualDemoFactory {
    reserve: AccountId,
    runner: AccountId,
    reviewers: [AccountId; 3],
    incident_recipient: AccountId,
    wallet_code_hash: String,
    accounts: LookupMap<AccountId, Setup>,
    risk_group: Option<String>,
}

#[near]
impl ManualDemoFactory {
    #[init]
    pub fn new(
        reserve: AccountId,
        runner: AccountId,
        reviewers: [AccountId; 3],
        incident_recipient: AccountId,
        wallet_code_hash: String,
        risk_group: Option<String>,
    ) -> Self {
        let factory = env::current_account_id();
        // Leave space for the vault and its largest agent subaccount name.
        assert!(factory.as_str().ends_with(".testnet") && factory.as_str().len() <= 24);
        assert!(
            reviewers[0] != reviewers[1]
                && reviewers[0] != reviewers[2]
                && reviewers[1] != reviewers[2]
        );
        assert!(reviewers.iter().all(|id| id != &runner));
        Self {
            reserve,
            runner,
            reviewers,
            incident_recipient,
            wallet_code_hash,
            accounts: LookupMap::new(b"s"),
            risk_group,
        }
    }

    pub fn pool_config(&self) -> Option<(AccountId, String)> {
        self.risk_group
            .as_ref()
            .map(|group| (self.reserve.clone(), group.clone()))
    }

    pub fn setup_deposit(&self) -> NearToken {
        Self::contract_funding(WALLET_CODE)
            .checked_add(Self::contract_funding(VAULT_CODE))
            .and_then(|amount| amount.checked_add(REGISTRY_FEE))
            .expect("Setup deposit overflow")
    }

    pub fn setup(&self, owner: AccountId) -> Option<Setup> {
        self.accounts.get(&owner).cloned()
    }

    #[payable]
    pub fn prepare(&mut self) -> Promise {
        assert!(
            env::prepaid_gas() >= Gas::from_tgas(255),
            "Attach at least 255 Tgas"
        );
        let owner = env::predecessor_account_id();
        assert_eq!(
            owner,
            env::signer_account_id(),
            "Sign directly with the owner wallet"
        );
        assert!(
            owner.as_str().ends_with(".testnet") && !self.reviewers.contains(&owner),
            "Use a named testnet wallet"
        );
        let mut setup = if let Some(existing) = self.accounts.get(&owner) {
            assert_eq!(
                env::attached_deposit(),
                NO_DEPOSIT,
                "Existing setup has already been funded"
            );
            assert!(
                !existing.pending && !matches!(existing.stage, Stage::Ready),
                "Setup is already pending or ready"
            );
            existing.clone()
        } else {
            assert_eq!(
                env::attached_deposit(),
                self.setup_deposit(),
                "Incorrect setup deposit"
            );
            let parent = env::current_account_id();
            let hash = env::sha256(owner.as_bytes());
            let suffix: String = hash[..10]
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            Setup {
                wallet: format!("w-{suffix}.{parent}")
                    .parse()
                    .expect("Wallet account"),
                vault: format!("v-{suffix}.{parent}")
                    .parse()
                    .expect("Vault account"),
                stage: Stage::Wallet,
                pending: false,
            }
        };
        setup.pending = true;
        self.accounts.insert(owner.clone(), setup.clone());
        self.continue_setup(owner, &setup)
    }

    #[private]
    pub fn finish_step(
        &mut self,
        owner: AccountId,
        #[callback_result] result: Result<(), PromiseError>,
    ) -> PromiseOrValue<bool> {
        let setup = self.accounts.get_mut(&owner).expect("Missing setup");
        assert!(setup.pending, "Setup is not pending");
        if result.is_err() {
            setup.pending = false;
            return PromiseOrValue::Value(false);
        }
        setup.stage = match setup.stage {
            Stage::Wallet => Stage::Vault,
            Stage::Vault => Stage::Register,
            Stage::Register => Stage::Ready,
            Stage::Ready => env::panic_str("Setup already complete"),
        };
        if matches!(setup.stage, Stage::Ready) {
            setup.pending = false;
            return PromiseOrValue::Value(true);
        }
        let setup = setup.clone();
        PromiseOrValue::Promise(self.continue_setup(owner, &setup))
    }

    fn contract_funding(code: &[u8]) -> NearToken {
        let bytes = code.len() as u128 + 30_000;
        NearToken::from_yoctonear(bytes * env::storage_byte_cost().as_yoctonear())
    }

    fn install(account: AccountId, code: &[u8], args: Value) -> Promise {
        Promise::new(account)
            .create_account()
            .transfer(Self::contract_funding(code))
            .deploy_contract(code.to_vec())
            .function_call(
                "new",
                args.to_string().into_bytes(),
                NO_DEPOSIT,
                Gas::from_tgas(15),
            )
    }

    fn continue_setup(&self, owner: AccountId, setup: &Setup) -> Promise {
        let (step, callback_gas) = match setup.stage {
            Stage::Wallet => (
                Self::install(
                    setup.wallet.clone(),
                    WALLET_CODE,
                    json!({
                        "vault": setup.vault, "runner": self.runner, "recipient": self.incident_recipient,
                    }),
                ),
                170,
            ),
            Stage::Vault => (
                Self::install(
                    setup.vault.clone(),
                    VAULT_CODE,
                    json!({
                        "owner": owner, "wallet": setup.wallet, "runner": self.runner, "beneficiary": owner,
                        "reviewers": self.reviewers, "reserve_pool": self.reserve, "wallet_code_hash": self.wallet_code_hash,
                        "rolling": self.risk_group.is_some(), "capital_pilot": self.risk_group.is_some(),
                    }),
                ),
                95,
            ),
            Stage::Register => (
                Promise::new(self.reserve.clone()).function_call(
                    if self.risk_group.is_some() {
                        "register_vault_group"
                    } else {
                        "register_vault"
                    },
                    json!({"vault": setup.vault, "beneficiary": owner, "group": self.risk_group})
                        .to_string()
                        .into_bytes(),
                    NO_DEPOSIT,
                    Gas::from_tgas(22),
                ),
                20,
            ),
            Stage::Ready => env::panic_str("Setup already complete"),
        };
        step.then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(callback_gas))
                .finish_step(owner),
        )
    }
}
