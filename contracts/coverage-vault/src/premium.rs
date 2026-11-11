use super::*;

#[near]
impl CoverageVault {
    pub fn settle_earned_premiums(&mut self, policy_id: u32) -> Promise {
        assert!(
            self.capital_pilot,
            "This vault does not use the capital pilot"
        );
        assert!(
            !self.premiums_pending && !self.premiums_distributed,
            "Premium distribution already started"
        );
        let policy = self.policy.as_ref().expect("No policy");
        assert!(
            policy.id == policy_id && matches!(policy.phase, Phase::Closed | Phase::Paid),
            "Finish the policy first"
        );
        assert!(
            Self::now() >= self.term_end(policy),
            "The policy term has not ended"
        );
        assert!(
            self.renewal.is_none()
                && !self.premium_refund_pending
                && self.unused_window().is_none(),
            "Reconcile premiums and refunds first"
        );
        let amount = policy
            .paid_premiums
            .iter()
            .try_fold(ZERO, |sum, amount| sum.checked_add(*amount))
            .expect("Premium overflow");
        let cashback_eligible = self.retired_at_ms.is_none()
            && policy.windows.iter().enumerate().all(|(index, start)| {
                *start == Some(Self::window_start(policy, self.window_seconds, index))
            });
        self.premiums_pending = true;
        Promise::new(self.reserve_pool.clone())
            .function_call(
                "receive_earned_premiums".to_owned(),
                near_sdk::serde_json::to_vec(
                    &near_sdk::serde_json::json!({ "policy_id": policy_id,
                "owner": self.owner, "cashback_eligible": cashback_eligible }),
                )
                .expect("Premium parameters"),
                amount,
                Gas::from_tgas(36),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(12))
                    .finish_earned_premiums(),
            )
    }

    #[private]
    pub fn finish_earned_premiums(&mut self) -> bool {
        assert!(self.premiums_pending, "No pending premium distribution");
        self.premiums_pending = false;
        self.premiums_distributed = env::promise_result_checked(0, 0).is_ok();
        self.premiums_distributed
    }
}
