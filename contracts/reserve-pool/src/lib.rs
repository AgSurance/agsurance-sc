use near_sdk::store::{LookupMap, LookupSet};
use near_sdk::{env, near, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};

mod capital;
use capital::CapitalPilot;

const ZERO: NearToken = NearToken::from_yoctonear(0);
const LIMIT: NearToken = NearToken::from_millinear(400);

#[near(serializers = [borsh, json])]
enum Allocation {
    Reserved,
    Paying(NearToken),
    Paid(NearToken),
}

#[near(serializers = [json])]
pub enum Payment {
    Pending,
    Retry,
    Paid,
}

#[near(serializers = [json])]
pub struct ReserveStatus {
    assets: NearToken,
    locked: NearToken,
    available: NearToken,
    shortfall: NearToken,
    pending: NearToken,
    quarantined: NearToken,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct PoolLimits {
    max_policy: NearToken,
    max_group: NearToken,
    registrar: AccountId,
}

#[near(serializers = [json])]
pub struct PoolCapacity {
    limits: PoolLimits,
    group_remaining: NearToken,
    liquid_available: NearToken,
    issuing: bool,
    buffer: NearToken,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct ReservePool {
    operator: AccountId,
    recipients: LookupMap<AccountId, AccountId>,
    allocations: LookupMap<(AccountId, u32), Allocation>,
    assets: NearToken,
    locked: NearToken,
    pending: NearToken,
    quarantined: NearToken,
    capital: Option<CapitalPilot>,
    open_premium_accounts: u32,
    beneficiary_locked: LookupSet<AccountId>,
    limits: Option<PoolLimits>,
    groups: LookupMap<AccountId, String>,
    group_locked: LookupMap<String, NearToken>,
    policy_limits: LookupMap<(AccountId, u32), NearToken>,
}

#[near]
impl ReservePool {
    #[init]
    pub fn new(operator: AccountId) -> Self {
        assert!(env::current_account_id().as_str().ends_with(".testnet"));
        Self {
            operator,
            recipients: LookupMap::new(b"r"),
            allocations: LookupMap::new(b"a"),
            assets: ZERO,
            locked: ZERO,
            pending: ZERO,
            quarantined: ZERO,
            capital: None,
            open_premium_accounts: 0,
            beneficiary_locked: LookupSet::new(b"b"),
            limits: None,
            groups: LookupMap::new(b"g"),
            group_locked: LookupMap::new(b"e"),
            policy_limits: LookupMap::new(b"l"),
        }
    }

    pub fn reserve_status(&self) -> ReserveStatus {
        ReserveStatus {
            assets: self.assets,
            locked: self.locked,
            available: self
                .assets
                .checked_sub(self.locked)
                .and_then(|amount| {
                    amount.checked_sub(self.capital.as_ref().map_or(ZERO, CapitalPilot::buffer))
                })
                .unwrap_or(ZERO),
            shortfall: self.locked.checked_sub(self.assets).unwrap_or(ZERO),
            pending: self.pending,
            quarantined: self.quarantined,
        }
    }

    pub fn vault_beneficiary(&self, vault: AccountId) -> Option<AccountId> {
        self.recipients.get(&vault).cloned()
    }

    pub fn register_vault(&mut self, vault: AccountId, beneficiary: AccountId) {
        let registrar = self.limits.as_ref().map(|limits| &limits.registrar);
        let caller = env::predecessor_account_id();
        assert!(
            caller == self.operator || registrar == Some(&caller),
            "Authorized vault registrar required"
        );
        assert!(vault.as_str().ends_with(".testnet") && beneficiary.as_str().ends_with(".testnet"));
        assert_ne!(
            beneficiary,
            env::current_account_id(),
            "Invalid beneficiary"
        );
        assert!(
            !self.recipients.contains_key(&vault),
            "Vault already registered"
        );
        self.recipients.insert(vault, beneficiary);
    }

    pub fn configure_rolling_limits(
        &mut self,
        max_policy: NearToken,
        max_group: NearToken,
        registrar: AccountId,
    ) {
        self.assert_operator();
        assert!(
            self.limits.is_none() && self.assets == ZERO && self.locked == ZERO,
            "Configure an empty pool"
        );
        assert!(
            self.capital.as_ref().is_some_and(CapitalPilot::is_unfunded),
            "Configure capital first"
        );
        assert!(
            max_policy > ZERO && max_group >= max_policy,
            "Invalid underwriting limits"
        );
        self.limits = Some(PoolLimits {
            max_policy,
            max_group,
            registrar,
        });
    }

    pub fn register_vault_group(
        &mut self,
        vault: AccountId,
        beneficiary: AccountId,
        group: String,
    ) {
        assert!(self.limits.is_some(), "Dynamic pool required");
        assert!(!group.is_empty() && group.len() <= 64, "Invalid risk group");
        self.register_vault(vault.clone(), beneficiary);
        self.groups.insert(vault, group);
    }

    pub fn pool_capacity(&self, group: String) -> Option<PoolCapacity> {
        let limits = self.limits.clone()?;
        Some(PoolCapacity {
            group_remaining: limits
                .max_group
                .saturating_sub(self.group_locked.get(&group).copied().unwrap_or(ZERO)),
            limits,
            liquid_available: self.reserve_status().available,
            issuing: self.capital.as_ref().is_some_and(CapitalPilot::can_issue),
            buffer: self.capital.as_ref().map_or(ZERO, CapitalPilot::buffer),
        })
    }

    pub fn set_initial_beneficiary(&mut self, beneficiary: AccountId) {
        let vault = env::predecessor_account_id();
        assert!(
            self.recipients.contains_key(&vault),
            "Registered vault required"
        );
        if self.recipients.get(&vault) == Some(&beneficiary) {
            return;
        }
        assert!(
            !self.beneficiary_locked.contains(&vault),
            "Beneficiary is locked"
        );
        assert!(
            beneficiary.as_str().ends_with(".testnet") && beneficiary != env::current_account_id(),
            "Invalid beneficiary"
        );
        self.beneficiary_locked.insert(vault.clone());
        self.recipients.insert(vault, beneficiary);
    }

    #[payable]
    pub fn fund_reserve(&mut self) {
        if self
            .capital
            .as_mut()
            .is_some_and(|pilot| pilot.add_closed_principal(env::attached_deposit()))
        {
            return;
        }
        self.assets = self
            .assets
            .checked_add(env::attached_deposit())
            .expect("Assets overflow");
    }

    // Testnet scenario only. Impaired collateral remains in custody but can no longer back claims.
    pub fn quarantine_demo_collateral(&mut self, amount: NearToken) {
        self.assert_operator();
        assert_eq!(self.pending, ZERO, "Reconcile pending payouts first");
        self.assets = self
            .assets
            .checked_sub(amount)
            .expect("Amount exceeds assets");
        self.quarantined = self
            .quarantined
            .checked_add(amount)
            .expect("Collateral overflow");
    }

    pub fn reserve_policy(&mut self, policy_id: u32, limit: Option<NearToken>) -> bool {
        let vault = env::predecessor_account_id();
        assert!(
            self.recipients.contains_key(&vault),
            "Registered vault required"
        );
        let key = (vault.clone(), policy_id);
        let limit = if self.limits.is_some() {
            limit.expect("Choose a policy limit")
        } else {
            assert!(
                limit.is_none() || limit == Some(LIMIT),
                "Fixed limit required"
            );
            LIMIT
        };
        if let Some(allocation) = self.allocations.get(&key) {
            return matches!(allocation, Allocation::Reserved) && self.policy_limit(&key) == limit;
        }
        if !self.add_exposure(&vault, ZERO, limit) {
            return false;
        }
        self.policy_limits.insert(key.clone(), limit);
        self.allocations.insert(key, Allocation::Reserved);
        self.beneficiary_locked.insert(vault);
        if self.capital.is_some() {
            self.open_premium_accounts += 1;
        }
        true
    }

    pub fn increase_policy_limit(
        &mut self,
        policy_id: u32,
        previous: NearToken,
        limit: NearToken,
    ) -> bool {
        assert!(
            self.limits.is_some() && limit > previous,
            "Invalid increase"
        );
        let vault = env::predecessor_account_id();
        let key = (vault.clone(), policy_id);
        if !matches!(self.allocations.get(&key), Some(Allocation::Reserved)) {
            return false;
        }
        let current = self.policy_limit(&key);
        if current == limit {
            return true;
        }
        if current != previous || !self.add_exposure(&vault, previous, limit) {
            return false;
        }
        self.policy_limits.insert(key, limit);
        true
    }

    fn add_exposure(&mut self, vault: &AccountId, previous: NearToken, limit: NearToken) -> bool {
        let delta = limit.saturating_sub(previous);
        if delta == ZERO
            || self
                .capital
                .as_ref()
                .is_some_and(|pilot| !pilot.can_issue())
            || delta > self.reserve_status().available
        {
            return false;
        }
        if let Some(limits) = &self.limits {
            let group = self
                .groups
                .get(vault)
                .expect("Registered risk group required")
                .clone();
            let exposure = self.group_locked.get(&group).copied().unwrap_or(ZERO);
            if limit > limits.max_policy || delta > limits.max_group.saturating_sub(exposure) {
                return false;
            }
            self.group_locked.insert(
                group,
                exposure
                    .checked_add(delta)
                    .expect("Group allocation overflow"),
            );
        }
        self.locked = self.locked.checked_add(delta).expect("Allocation overflow");
        true
    }

    fn policy_limit(&self, key: &(AccountId, u32)) -> NearToken {
        self.policy_limits.get(key).copied().unwrap_or(LIMIT)
    }

    fn release_exposure(&mut self, key: &(AccountId, u32)) {
        let limit = self.policy_limit(key);
        self.policy_limits.remove(key);
        self.locked = self.locked.checked_sub(limit).expect("Missing allocation");
        if let Some(group) = self.groups.get(&key.0) {
            let exposure = self
                .group_locked
                .get_mut(group)
                .expect("Missing group allocation");
            *exposure = exposure.checked_sub(limit).expect("Missing group exposure");
        }
    }

    pub fn release_failed_checkout(&mut self, policy_id: u32) {
        let key = (env::predecessor_account_id(), policy_id);
        if self.capital.is_some()
            && matches!(self.allocations.get(&key), Some(Allocation::Reserved))
        {
            self.open_premium_accounts -= 1;
        }
        self.release_closed_policy(policy_id);
    }

    pub fn release_closed_policy(&mut self, policy_id: u32) {
        let vault = env::predecessor_account_id();
        assert!(
            self.recipients.contains_key(&vault),
            "Registered vault required"
        );
        let key = (vault, policy_id);
        match self.allocations.get(&key) {
            None => {}
            Some(Allocation::Reserved) => {
                self.allocations.remove(&key);
                self.release_exposure(&key);
            }
            _ => env::panic_str("Allocation cannot be released"),
        }
    }

    pub fn settle_claim(&mut self, policy_id: u32, amount: NearToken) -> PromiseOrValue<Payment> {
        let vault = env::predecessor_account_id();
        let beneficiary = self
            .recipients
            .get(&vault)
            .expect("Registered vault required")
            .clone();
        let key = (vault.clone(), policy_id);
        match self.allocations.get(&key).expect("Allocation required") {
            Allocation::Paid(previous) => {
                assert_eq!(amount, *previous, "Payout amount changed");
                return PromiseOrValue::Value(Payment::Paid);
            }
            Allocation::Paying(previous) => {
                assert_eq!(amount, *previous, "Payout amount changed");
                return PromiseOrValue::Value(Payment::Pending);
            }
            Allocation::Reserved => {}
        }
        assert!(amount <= self.policy_limit(&key), "Invalid payout amount");
        if amount.is_zero() {
            self.release_exposure(&key);
            self.allocations.insert(key, Allocation::Paid(ZERO));
            return PromiseOrValue::Value(Payment::Paid);
        }
        if self.assets < self.locked {
            return PromiseOrValue::Value(Payment::Retry);
        }

        self.pending = self.pending.checked_add(amount).expect("Pending overflow");
        self.allocations.insert(key, Allocation::Paying(amount));
        PromiseOrValue::Promise(
            Promise::new(beneficiary).transfer(amount).then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(14))
                    .finish_transfer(vault, policy_id),
            ),
        )
    }

    #[private]
    pub fn finish_transfer(&mut self, vault: AccountId, policy_id: u32) -> Payment {
        let key = (vault, policy_id);
        let Some(Allocation::Paying(amount)) = self.allocations.get(&key) else {
            env::panic_str("No pending payout");
        };
        let amount = *amount;
        self.pending = self
            .pending
            .checked_sub(amount)
            .expect("Missing pending amount");
        if env::promise_result_checked(0, 0).is_err() {
            self.allocations.insert(key, Allocation::Reserved);
            return Payment::Retry;
        }
        self.release_exposure(&key);
        self.allocations.insert(key, Allocation::Paid(amount));
        self.assets = self.assets.checked_sub(amount).expect("Missing assets");
        if let Some(pilot) = &mut self.capital {
            pilot.record_claim_payout(amount);
        }
        Payment::Paid
    }

    fn assert_operator(&self) {
        assert_eq!(
            env::predecessor_account_id(),
            self.operator,
            "Operator required"
        );
    }
}
