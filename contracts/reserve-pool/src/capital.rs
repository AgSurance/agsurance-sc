use super::*;
use near_sdk::PromiseError;

const MAX_COHORT_CAPITAL: u128 = 100_000;
const UNIT: u128 = NearToken::from_millinear(1).as_yoctonear();
const BPS: u128 = 10_000;

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ValidatorState {
    Idle,
    Staking,
    Staked,
    Unstaking,
    Unbonding,
    Checking,
    Withdrawing,
    AwaitingTransfer,
}

#[near(serializers = [borsh])]
pub(super) struct CapitalPilot {
    validator: AccountId,
    buffer: NearToken,
    delegation_cap: NearToken,
    capital_bps: u16,
    cashback_bps: u16,
    sealed: bool,
    closed: bool,
    units: u128,
    providers: LookupMap<AccountId, u128>,
    claimed: LookupMap<AccountId, NearToken>,
    credited_policies: LookupMap<(AccountId, u32), ()>,
    credits: LookupMap<AccountId, NearToken>,
    pending_credits: LookupMap<AccountId, NearToken>,
    total_credits: NearToken,
    credit_transfers: u32,
    claims_paid: NearToken,
    premium_income: NearToken,
    staking_income: NearToken,
    distributable_principal: NearToken,
    principal_delegated: NearToken,
    validator_state: ValidatorState,
    withdrawal: NearToken,
    withdrawal_baseline: NearToken,
}

#[near(serializers = [json])]
pub struct CapitalStatus {
    validator: AccountId,
    buffer: NearToken,
    liquid_surplus: NearToken,
    liquidity_gap: NearToken,
    delegation_cap: NearToken,
    sealed: bool,
    closed: bool,
    capital_raised: NearToken,
    claims_paid: NearToken,
    premium_income: NearToken,
    staking_income: NearToken,
    principal_delegated: NearToken,
    net_principal_loss: NearToken,
    validator_state: ValidatorState,
    withdrawable_income: NearToken,
    account_share: Option<CapitalShare>,
}

#[near(serializers = [json])]
struct CapitalShare {
    principal_contributed: NearToken,
    claim_payouts: NearToken,
    premiums: NearToken,
    staking_rewards: NearToken,
}

fn share(amount: NearToken, numerator: u128, denominator: u128) -> NearToken {
    // Split the integer and fractional portions first to avoid multiplying a NEAR
    // balance by all the cohort units in one operation.
    let value = amount.as_yoctonear();
    NearToken::from_yoctonear(
        value / denominator * numerator + (value % denominator) * numerator / denominator,
    )
}

impl CapitalPilot {
    fn new(
        validator: AccountId,
        buffer: NearToken,
        delegation_cap: NearToken,
        capital_bps: u16,
        cashback_bps: u16,
    ) -> Self {
        Self {
            validator,
            buffer,
            delegation_cap,
            capital_bps,
            cashback_bps,
            sealed: false,
            closed: false,
            units: 0,
            providers: LookupMap::new(&b"capital:providers"[..]),
            claimed: LookupMap::new(&b"capital:claimed"[..]),
            credited_policies: LookupMap::new(&b"capital:policies"[..]),
            credits: LookupMap::new(&b"capital:credits"[..]),
            pending_credits: LookupMap::new(&b"capital:pending"[..]),
            total_credits: ZERO,
            credit_transfers: 0,
            claims_paid: ZERO,
            premium_income: ZERO,
            staking_income: ZERO,
            distributable_principal: ZERO,
            principal_delegated: ZERO,
            validator_state: ValidatorState::Idle,
            withdrawal: ZERO,
            withdrawal_baseline: ZERO,
        }
    }

    pub(super) fn can_issue(&self) -> bool {
        self.sealed && !self.closed
    }

    pub(super) fn buffer(&self) -> NearToken {
        if self.closed {
            ZERO
        } else {
            self.buffer
        }
    }

    pub(super) fn add_closed_principal(&mut self, amount: NearToken) -> bool {
        if !self.closed {
            return false;
        }
        self.distributable_principal = self
            .distributable_principal
            .checked_add(amount)
            .expect("Principal overflow");
        true
    }

    pub(super) fn record_claim_payout(&mut self, amount: NearToken) {
        self.claims_paid = self
            .claims_paid
            .checked_add(amount)
            .expect("Claim total overflow");
    }

    fn credit(&mut self, account: AccountId, amount: NearToken) {
        if amount == ZERO {
            return;
        }
        let balance = self.credits.entry(account).or_insert(ZERO);
        *balance = balance.checked_add(amount).expect("Credit overflow");
        self.total_credits = self
            .total_credits
            .checked_add(amount)
            .expect("Credit overflow");
    }
}

#[near]
impl ReservePool {
    pub fn configure_capital_pilot(
        &mut self,
        validator: AccountId,
        buffer: NearToken,
        delegation_cap: NearToken,
        capital_bps: u16,
        cashback_bps: u16,
    ) {
        self.assert_operator();
        assert!(
            self.capital.is_none() && self.assets == ZERO && self.locked == ZERO,
            "Configure an empty pool"
        );
        assert!(validator != env::current_account_id(), "Invalid validator");
        assert!(
            buffer > ZERO && delegation_cap > ZERO,
            "Set a positive buffer and delegation cap"
        );
        assert!(
            u128::from(capital_bps) <= BPS && u128::from(cashback_bps) <= BPS,
            "Invalid premium percentages"
        );
        self.capital = Some(CapitalPilot::new(
            validator,
            buffer,
            delegation_cap,
            capital_bps,
            cashback_bps,
        ));
    }

    #[payable]
    pub fn fund_capital_pilot(&mut self) {
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(!pilot.sealed, "Capital cohort is sealed");
        let amount = env::attached_deposit();
        let yocto = amount.as_yoctonear();
        assert!(
            yocto >= UNIT && yocto % UNIT == 0,
            "Deposit a whole number of milliNEAR"
        );
        let units = yocto / UNIT;
        assert!(
            units <= MAX_COHORT_CAPITAL * 1000 - pilot.units,
            "Pilot capital cap exceeded"
        );
        *pilot
            .providers
            .entry(env::predecessor_account_id())
            .or_insert(0) += units;
        pilot.units += units;
        self.assets = self.assets.checked_add(amount).expect("Assets overflow");
    }

    pub fn seal_capital_pilot(&mut self) {
        self.assert_operator();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(
            !pilot.sealed && pilot.units > 0 && self.assets >= pilot.buffer,
            "Fund the pilot and buffer first"
        );
        pilot.sealed = true;
    }

    pub fn capital_status(&self, account_id: Option<AccountId>) -> Option<CapitalStatus> {
        let pilot = self.capital.as_ref()?;
        let capital_raised = NearToken::from_yoctonear(pilot.units * UNIT);
        let principal = if pilot.closed {
            pilot.distributable_principal
        } else {
            self.assets
                .checked_add(pilot.principal_delegated)
                .expect("Principal overflow")
        };
        Some(CapitalStatus {
            validator: pilot.validator.clone(),
            buffer: pilot.buffer,
            liquid_surplus: self.reserve_status().available,
            liquidity_gap: self
                .locked
                .checked_add(pilot.buffer())
                .expect("Allocation overflow")
                .checked_sub(self.assets)
                .unwrap_or(ZERO),
            delegation_cap: pilot.delegation_cap,
            sealed: pilot.sealed,
            closed: pilot.closed,
            capital_raised,
            claims_paid: pilot.claims_paid,
            premium_income: pilot.premium_income,
            staking_income: pilot.staking_income,
            principal_delegated: pilot.principal_delegated,
            net_principal_loss: capital_raised.checked_sub(principal).unwrap_or(ZERO),
            validator_state: pilot.validator_state,
            account_share: account_id
                .as_ref()
                .and_then(|id| pilot.providers.get(id))
                .map(|units| CapitalShare {
                    principal_contributed: NearToken::from_yoctonear(*units * UNIT),
                    claim_payouts: share(pilot.claims_paid, *units, pilot.units),
                    premiums: share(pilot.premium_income, *units, pilot.units),
                    staking_rewards: share(pilot.staking_income, *units, pilot.units),
                }),
            withdrawable_income: account_id
                .and_then(|id| pilot.credits.get(&id).copied())
                .unwrap_or(ZERO),
        })
    }

    #[payable]
    pub fn receive_earned_premiums(
        &mut self,
        policy_id: u32,
        owner: AccountId,
        cashback_eligible: bool,
    ) {
        let vault = env::predecessor_account_id();
        assert!(
            self.recipients.contains_key(&vault),
            "Registered vault required"
        );
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(pilot.sealed && !pilot.closed, "Capital cohort is not open");
        let key = (vault, policy_id);
        assert!(
            !pilot.credited_policies.contains_key(&key),
            "Premium already distributed"
        );
        let premium = env::attached_deposit();
        assert!(premium > ZERO, "No earned premiums");
        let cashback = if cashback_eligible {
            share(premium, u128::from(pilot.cashback_bps), BPS)
        } else {
            ZERO
        };
        let net = premium
            .checked_sub(cashback)
            .expect("Cashback exceeds premium");
        let capital = share(net, u128::from(pilot.capital_bps), BPS);
        pilot.premium_income = pilot
            .premium_income
            .checked_add(capital)
            .expect("Income overflow");
        pilot.credit(owner, cashback);
        pilot.credit(
            self.operator.clone(),
            net.checked_sub(capital)
                .expect("Capital exceeds net premium"),
        );
        pilot.credited_policies.insert(key, ());
        self.open_premium_accounts = self
            .open_premium_accounts
            .checked_sub(1)
            .expect("Missing premium account");
    }

    pub fn withdraw_pilot_income(&mut self) -> Promise {
        let account = env::predecessor_account_id();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(
            self.assets
                >= self
                    .locked
                    .checked_add(pilot.buffer())
                    .expect("Allocation overflow"),
            "Restore reserve liquidity first"
        );
        assert!(
            !pilot.pending_credits.contains_key(&account),
            "Withdrawal pending"
        );
        let amount = pilot
            .credits
            .remove(&account)
            .expect("No available balance");
        pilot.pending_credits.insert(account.clone(), amount);
        pilot.credit_transfers += 1;
        Promise::new(account.clone()).transfer(amount).then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(12))
                .finish_pilot_income(account),
        )
    }

    #[private]
    pub fn finish_pilot_income(&mut self, account: AccountId) -> bool {
        let pilot = self.capital.as_mut().expect("Missing capital pilot");
        let amount = pilot
            .pending_credits
            .remove(&account)
            .expect("No pending income");
        pilot.credit_transfers -= 1;
        if env::promise_result_checked(0, 0).is_err() {
            let available = pilot.credits.entry(account).or_insert(ZERO);
            *available = available.checked_add(amount).expect("Credit overflow");
            return false;
        }
        pilot.total_credits = pilot
            .total_credits
            .checked_sub(amount)
            .expect("Missing credit");
        true
    }

    pub fn delegate_surplus(&mut self, amount: NearToken) -> Promise {
        self.assert_operator();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(
            pilot.can_issue() && pilot.validator_state == ValidatorState::Idle,
            "Finish the current delegation first"
        );
        let floor = self
            .locked
            .checked_add(pilot.buffer)
            .expect("Allocation overflow");
        let available = self.assets.checked_sub(floor).unwrap_or(ZERO);
        assert!(
            amount > ZERO && amount <= available && amount <= pilot.delegation_cap,
            "Delegation exceeds liquid surplus or cap"
        );
        self.assets = self.assets.checked_sub(amount).expect("Missing reserve");
        pilot.principal_delegated = amount;
        pilot.validator_state = ValidatorState::Staking;
        Self::validator_call(
            &pilot.validator,
            "deposit_and_stake",
            near_sdk::serde_json::json!({}),
            amount,
            100,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(15))
                .finish_delegation(),
        )
    }

    #[private]
    pub fn finish_delegation(&mut self) -> bool {
        let pilot = self.capital.as_mut().expect("Missing capital pilot");
        assert!(
            pilot.validator_state == ValidatorState::Staking,
            "No pending delegation"
        );
        if env::promise_result_checked(0, 0).is_err() {
            self.assets = self
                .assets
                .checked_add(pilot.principal_delegated)
                .expect("Assets overflow");
            pilot.principal_delegated = ZERO;
            pilot.validator_state = ValidatorState::Idle;
            return false;
        }
        pilot.validator_state = ValidatorState::Staked;
        true
    }

    pub fn request_undelegation(&mut self) -> Promise {
        self.assert_operator();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(
            pilot.validator_state == ValidatorState::Staked,
            "No active delegation"
        );
        pilot.validator_state = ValidatorState::Unstaking;
        Self::validator_call(
            &pilot.validator,
            "unstake_all",
            near_sdk::serde_json::json!({}),
            ZERO,
            100,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(15))
                .finish_undelegation(),
        )
    }

    #[private]
    pub fn finish_undelegation(&mut self) -> bool {
        let pilot = self.capital.as_mut().expect("Missing capital pilot");
        assert!(
            pilot.validator_state == ValidatorState::Unstaking,
            "No pending undelegation"
        );
        let ok = env::promise_result_checked(0, 0).is_ok();
        pilot.validator_state = if ok {
            ValidatorState::Unbonding
        } else {
            ValidatorState::Staked
        };
        ok
    }

    pub fn request_validator_withdrawal(&mut self) -> Promise {
        self.assert_operator();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(
            pilot.validator_state == ValidatorState::Unbonding,
            "No unbonding delegation"
        );
        pilot.validator_state = ValidatorState::Checking;
        Self::validator_call(
            &pilot.validator,
            "get_account_unstaked_balance",
            near_sdk::serde_json::json!({ "account_id": env::current_account_id() }),
            ZERO,
            12,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(145))
                .withdraw_from_validator(),
        )
    }

    #[private]
    pub fn withdraw_from_validator(
        &mut self,
        #[callback_result] result: Result<NearToken, PromiseError>,
    ) -> PromiseOrValue<bool> {
        let excess = self.validator_untracked_balance();
        let pilot = self.capital.as_mut().expect("Missing capital pilot");
        assert!(
            pilot.validator_state == ValidatorState::Checking,
            "No pending balance check"
        );
        let (Some(baseline), Ok(amount)) = (excess, result) else {
            pilot.validator_state = ValidatorState::Unbonding;
            return PromiseOrValue::Value(false);
        };
        if amount == ZERO {
            pilot.validator_state = ValidatorState::Unbonding;
            return PromiseOrValue::Value(false);
        }
        pilot.withdrawal_baseline = baseline;
        pilot.withdrawal = amount;
        pilot.validator_state = ValidatorState::Withdrawing;
        PromiseOrValue::Promise(
            Self::validator_call(
                &pilot.validator,
                "withdraw",
                near_sdk::serde_json::json!({ "amount": amount }),
                ZERO,
                100,
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(15))
                    .finish_validator_withdrawal(),
            ),
        )
    }

    #[private]
    pub fn finish_validator_withdrawal(&mut self) -> bool {
        let pilot = self.capital.as_mut().expect("Missing capital pilot");
        assert!(
            pilot.validator_state == ValidatorState::Withdrawing,
            "No pending validator withdrawal"
        );
        let ok = env::promise_result_checked(0, 0).is_ok();
        pilot.validator_state = if ok {
            ValidatorState::AwaitingTransfer
        } else {
            ValidatorState::Unbonding
        };
        ok
    }

    pub fn reconcile_validator_withdrawal(&mut self) -> bool {
        let excess = self.validator_untracked_balance();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(
            pilot.validator_state == ValidatorState::AwaitingTransfer,
            "No validator transfer to reconcile"
        );
        let required = pilot
            .withdrawal_baseline
            .checked_add(pilot.withdrawal)
            .expect("Withdrawal overflow");
        if excess.is_none_or(|balance| balance < required) {
            return false;
        }
        let principal = pilot.withdrawal.min(pilot.principal_delegated);
        let reward = pilot
            .withdrawal
            .checked_sub(principal)
            .expect("Missing principal");
        self.assets = self.assets.checked_add(principal).expect("Assets overflow");
        pilot.staking_income = pilot
            .staking_income
            .checked_add(reward)
            .expect("Reward overflow");
        pilot.principal_delegated = ZERO;
        pilot.withdrawal = ZERO;
        pilot.withdrawal_baseline = ZERO;
        pilot.validator_state = ValidatorState::Idle;
        true
    }

    pub fn close_capital_pilot(&mut self) {
        self.assert_operator();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(pilot.can_issue(), "Capital cohort is not open");
        assert_eq!(self.locked, ZERO, "Settle or close all policies first");
        assert_eq!(self.pending, ZERO, "Reconcile pending payouts first");
        assert!(
            pilot.validator_state == ValidatorState::Idle,
            "Reconcile validator funds first"
        );
        assert_eq!(
            self.open_premium_accounts, 0,
            "Distribute earned premiums first"
        );
        pilot.closed = true;
        pilot.distributable_principal = self.assets;
        self.assets = ZERO;
    }

    pub fn claim_capital_pilot(&mut self) {
        let account = env::predecessor_account_id();
        let pilot = self.capital.as_mut().expect("Configure the capital pilot");
        assert!(pilot.closed, "Close the capital cohort first");
        let units = *pilot.providers.get(&account).expect("No capital position");
        let principal = share(pilot.distributable_principal, units, pilot.units);
        let premiums = share(pilot.premium_income, units, pilot.units);
        let reward = share(pilot.staking_income, units, pilot.units);
        let total = principal
            .checked_add(premiums)
            .and_then(|amount| amount.checked_add(reward))
            .expect("Credit overflow");
        let previous = pilot.claimed.insert(account.clone(), total).unwrap_or(ZERO);
        pilot.credit(
            account,
            total
                .checked_sub(previous)
                .expect("Missing capital distribution"),
        );
    }

    fn validator_call(
        validator: &AccountId,
        method: &str,
        args: near_sdk::serde_json::Value,
        deposit: NearToken,
        gas: u64,
    ) -> Promise {
        Promise::new(validator.clone()).function_call(
            method,
            near_sdk::serde_json::to_vec(&args).expect("Validator arguments"),
            deposit,
            Gas::from_tgas(gas),
        )
    }

    fn validator_untracked_balance(&self) -> Option<NearToken> {
        let pilot = self.capital.as_ref()?;
        if self.pending != ZERO || pilot.credit_transfers != 0 {
            return None;
        }
        env::account_balance()
            .checked_sub(self.assets)?
            .checked_sub(self.quarantined)?
            .checked_sub(pilot.total_credits)?
            .checked_sub(pilot.premium_income)?
            .checked_sub(pilot.staking_income)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares_handle_rounding_and_full_balances() {
        let premium = NearToken::from_millinear(40);
        let cashback = share(premium, 2_000, BPS);
        assert_eq!(cashback, NearToken::from_millinear(8));
        let capital = share(premium.checked_sub(cashback).unwrap(), 7_000, BPS);
        assert_eq!(capital.as_yoctonear(), 22_400_000_000_000_000_000_000);
        assert_eq!(
            share(NearToken::from_yoctonear(u128::MAX), 1, 1).as_yoctonear(),
            u128::MAX
        );
        assert_eq!(
            share(NearToken::from_yoctonear(101), 2, 3).as_yoctonear(),
            67
        );
    }
}
