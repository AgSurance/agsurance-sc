use super::*;

#[near]
impl CoverageVault {
    pub fn choose_recovery_account(&mut self, beneficiary: AccountId) -> Promise {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        assert!(
            !self.beneficiary_chosen && self.policy_sequence == 0 && self.checkout.is_none(),
            "Choose recovery before checkout"
        );
        assert!(
            beneficiary.as_str().ends_with(".testnet")
                && beneficiary != self.wallet
                && beneficiary != env::current_account_id()
                && beneficiary != self.reserve_pool
                && !self.reviewers.contains(&beneficiary),
            "Invalid recovery account"
        );
        assert!(
            self.pending_beneficiary
                .as_ref()
                .is_none_or(|pending| pending == &beneficiary),
            "Finish the pending recovery change"
        );
        self.pending_beneficiary = Some(beneficiary.clone());
        self.pool_call(
            "set_initial_beneficiary",
            near_sdk::serde_json::json!({ "beneficiary": beneficiary }),
            20,
        )
        .then(
            Self::ext(env::current_account_id())
                .with_static_gas(Gas::from_tgas(15))
                .finish_choose_recovery_account(beneficiary),
        )
    }

    #[private]
    pub fn finish_choose_recovery_account(&mut self, beneficiary: AccountId) -> bool {
        if self.pending_beneficiary.as_ref() != Some(&beneficiary) {
            return self.beneficiary == beneficiary;
        }
        let success = env::promise_result_checked(0, 0).is_ok();
        // Keep the same target on failure so concurrent retries cannot change the pool
        // to one recipient while the vault confirms another.
        if success {
            self.beneficiary = beneficiary;
            self.pending_beneficiary = None;
            self.beneficiary_chosen = true;
        }
        success
    }
}
