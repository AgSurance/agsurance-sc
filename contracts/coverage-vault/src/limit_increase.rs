use super::*;

const MAX_INCREASES: usize = 16;
const RETRY_BLOCKS: u64 = 120;

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub(super) struct LimitChange {
    pub(super) limit: NearToken,
    pub(super) effective_height: u64,
}

impl Policy {
    pub(super) fn current_limit(&self) -> NearToken {
        self.limit_changes
            .last()
            .map_or(self.coverage_limit, |change| change.limit)
    }

    pub(super) fn limit_at(&self, height: u64) -> NearToken {
        self.limit_changes
            .iter()
            .rev()
            .find(|change| change.effective_height <= height)
            .map_or(self.coverage_limit, |change| change.limit)
    }
}

#[near(serializers = [borsh, json])]
#[derive(Clone, PartialEq, Eq)]
enum IncreaseStage {
    CheckingBalance,
    Reserving,
    Refunding,
    RefundAvailable,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub(super) struct LimitIncrease {
    id: u32,
    policy_id: u32,
    previous: NearToken,
    limit: NearToken,
    fee_per_window: NearToken,
    fee: NearToken,
    window: u8,
    stage: IncreaseStage,
    retry_height: u64,
}

#[near]
impl CoverageVault {
    #[payable]
    pub fn request_limit_increase(
        &mut self,
        policy_id: u32,
        expected_limit: NearToken,
        limit: NearToken,
    ) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        assert!(
            self.rolling && !self.paused && self.renewal.is_none(),
            "Limit increase unavailable"
        );
        self.assert_no_increase();
        let policy = self.policy.as_ref().expect("Active policy required");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Active policy required"
        );
        assert_eq!(
            policy.current_limit(),
            expected_limit,
            "Policy limit changed"
        );
        assert!(
            limit > expected_limit && policy.limit_changes.len() < MAX_INCREASES,
            "Invalid limit increase"
        );
        let coverage = self.coverage();
        assert!(coverage.active, "Coverage must be active");
        let window = coverage.current_window.expect("Current premium window");
        let fee_per_window = self
            .premium_for(limit)
            .saturating_sub(self.premium_for(expected_limit));
        let prepaid = policy.windows[usize::from(window)..]
            .iter()
            .flatten()
            .count() as u128;
        let fee = fee_per_window.checked_mul(prepaid).expect("Fee overflow");
        assert_eq!(env::attached_deposit(), fee, "Incorrect additional premium");
        self.increase_sequence = self
            .increase_sequence
            .checked_add(1)
            .expect("Increase ID overflow");
        self.limit_increase = Some(LimitIncrease {
            id: self.increase_sequence,
            policy_id,
            previous: expected_limit,
            limit,
            fee_per_window,
            fee,
            window,
            stage: IncreaseStage::CheckingBalance,
            retry_height: env::block_height().saturating_add(RETRY_BLOCKS),
        });
        self.check_increase_balance(self.increase_sequence)
    }

    fn check_increase_balance(&self, id: u32) -> Promise {
        let attempt = self
            .limit_increase
            .as_ref()
            .expect("Pending increase")
            .retry_height;
        self.wallet_balance().then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(72))
                .finish_increase_balance(id, attempt),
        )
    }

    pub(super) fn wallet_balance(&self) -> Promise {
        Promise::new(self.wallet.clone()).function_call(
            "operating_balance".to_string(),
            b"{}".to_vec(),
            ZERO,
            Gas::from_tgas(8),
        )
    }

    #[private]
    pub fn finish_increase_balance(
        &mut self,
        id: u32,
        attempt: u64,
        #[callback_result] balance: Result<NearToken, PromiseError>,
    ) -> PromiseOrValue<bool> {
        let Some(change) = self.limit_increase.as_ref().filter(|change| {
            change.id == id
                && change.retry_height == attempt
                && change.stage == IncreaseStage::CheckingBalance
        }) else {
            return PromiseOrValue::Value(false);
        };
        let coverage = self.coverage();
        if balance.is_err()
            || balance.unwrap().saturating_sub(DEDUCTIBLE) < change.limit
            || !coverage.active
            || coverage.current_window != Some(change.window)
        {
            return PromiseOrValue::Promise(self.refund_increase(false));
        }
        self.limit_increase
            .as_mut()
            .expect("Pending increase")
            .stage = IncreaseStage::Reserving;
        PromiseOrValue::Promise(self.reserve_increase())
    }

    fn reserve_increase(&self) -> Promise {
        let change = self.limit_increase.as_ref().expect("Pending increase");
        self.pool_call(
            "increase_policy_limit",
            near_sdk::serde_json::json!({
                "policy_id": change.policy_id, "previous": change.previous, "limit": change.limit,
            }),
            18,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(39))
                .finish_limit_increase(change.id, change.retry_height),
        )
    }

    #[private]
    pub fn finish_limit_increase(
        &mut self,
        id: u32,
        attempt: u64,
        #[callback_result] reserved: Result<bool, PromiseError>,
    ) -> PromiseOrValue<bool> {
        let Some(change) = self.limit_increase.as_ref().filter(|change| {
            change.id == id
                && change.retry_height == attempt
                && change.stage == IncreaseStage::Reserving
        }) else {
            return PromiseOrValue::Value(false);
        };
        match reserved {
            Ok(true) => {}
            Ok(false) => return PromiseOrValue::Promise(self.refund_increase(false)),
            // A retry uses the same target allocation and cannot reserve the delta twice.
            Err(_) => return PromiseOrValue::Value(false),
        }
        let policy = self.policy.as_mut().expect("Active policy required");
        for index in usize::from(change.window)..WINDOW_COUNT {
            if policy.windows[index].is_some() {
                policy.paid_premiums[index] = policy.paid_premiums[index]
                    .checked_add(change.fee_per_window)
                    .expect("Premium overflow");
            }
        }
        policy.limit_changes.push(LimitChange {
            limit: change.limit,
            effective_height: env::block_height().saturating_add(1),
        });
        self.limit_increase = None;
        PromiseOrValue::Value(true)
    }

    pub fn resume_limit_increase(&mut self, id: u32) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        let change = self.limit_increase.as_mut().expect("No pending increase");
        assert_eq!(change.id, id, "Stale request");
        match change.stage {
            IncreaseStage::RefundAvailable => self.refund_increase(true),
            IncreaseStage::Refunding => env::panic_str("Refund in progress"),
            _ => {
                assert!(
                    env::block_height() >= change.retry_height,
                    "Wait for the pending request"
                );
                change.retry_height = env::block_height().saturating_add(RETRY_BLOCKS);
                match change.stage {
                    IncreaseStage::CheckingBalance => self.check_increase_balance(id),
                    _ => self.reserve_increase(),
                }
            }
        }
    }

    fn refund_increase(&mut self, retry: bool) -> Promise {
        let change = self.limit_increase.as_mut().expect("Pending increase");
        change.stage = IncreaseStage::Refunding;
        Promise::new(self.owner.clone()).transfer(change.fee).then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(12))
                .finish_increase_refund(change.id, retry),
        )
    }

    #[private]
    pub fn finish_increase_refund(&mut self, id: u32, retry: bool) -> bool {
        let change = self.limit_increase.as_mut().expect("Pending increase");
        assert!(
            change.id == id && change.stage == IncreaseStage::Refunding,
            "No matching refund"
        );
        if env::promise_result_checked(0, 0).is_ok() {
            self.limit_increase = None;
            return retry;
        }
        change.stage = IncreaseStage::RefundAvailable;
        false
    }

    pub(super) fn assert_no_increase(&self) {
        assert!(
            self.limit_increase.is_none(),
            "Resolve the pending limit increase first"
        );
    }
}
