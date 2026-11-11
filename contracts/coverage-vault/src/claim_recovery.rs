use super::DEDUCTIBLE;
use near_sdk::{near, NearToken};

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy)]
pub enum RecoveryRecipient {
    Beneficiary,
    Reserve,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Default)]
pub struct ClaimRecovery {
    pub(super) recovered_before_payout: NearToken,
    recovered_after_payout: NearToken,
    beneficiary_claimable: NearToken,
    reserve_claimable: NearToken,
    beneficiary_pending: NearToken,
    reserve_pending: NearToken,
    beneficiary_over_limit: NearToken,
    beneficiary_deductible: NearToken,
    reserve_reimbursement: NearToken,
    #[serde(skip)]
    settled: Option<Settlement>,
}

#[near(serializers = [borsh])]
#[derive(Clone)]
struct Settlement {
    remaining_loss: NearToken,
    payout: NearToken,
}

impl ClaimRecovery {
    pub(super) fn payout(&self, loss: NearToken, limit: NearToken) -> NearToken {
        loss.saturating_sub(self.recovered_before_payout)
            .saturating_sub(DEDUCTIBLE)
            .min(limit)
    }

    pub(super) fn is_settled(&self) -> bool {
        self.settled.is_some()
    }

    pub(super) fn settle(&mut self, loss: NearToken, payout: NearToken) {
        self.settled = Some(Settlement {
            remaining_loss: loss.saturating_sub(self.recovered_before_payout),
            payout,
        });
    }

    pub(super) fn record(&mut self, amount: NearToken, loss: Option<NearToken>) {
        assert!(!amount.is_zero(), "Attach recovered NEAR");
        if let Some(settled) = &self.settled {
            let total = self.recovered_after_payout.saturating_add(amount);
            assert!(
                amount
                    <= settled
                        .remaining_loss
                        .saturating_sub(self.recovered_after_payout),
                "Recovery exceeds outstanding loss"
            );
            let over_limit = settled
                .remaining_loss
                .saturating_sub(DEDUCTIBLE)
                .saturating_sub(settled.payout);
            let first = total.min(over_limit);
            let reserve = total.saturating_sub(first).min(settled.payout);
            let deductible = total.saturating_sub(first).saturating_sub(reserve);
            self.beneficiary_claimable = self
                .beneficiary_claimable
                .saturating_add(first.saturating_sub(self.beneficiary_over_limit))
                .saturating_add(deductible.saturating_sub(self.beneficiary_deductible));
            self.reserve_claimable = self
                .reserve_claimable
                .saturating_add(reserve.saturating_sub(self.reserve_reimbursement));
            self.beneficiary_over_limit = first;
            self.beneficiary_deductible = deductible;
            self.reserve_reimbursement = reserve;
            self.recovered_after_payout = total;
        } else {
            assert!(
                amount
                    <= loss
                        .expect("Reviewable claim required")
                        .saturating_sub(self.recovered_before_payout),
                "Recovery exceeds outstanding loss"
            );
            self.recovered_before_payout = self.recovered_before_payout.saturating_add(amount);
            self.beneficiary_claimable = self.beneficiary_claimable.saturating_add(amount);
        }
    }

    fn balances(&mut self, recipient: RecoveryRecipient) -> (&mut NearToken, &mut NearToken) {
        match recipient {
            RecoveryRecipient::Beneficiary => (
                &mut self.beneficiary_claimable,
                &mut self.beneficiary_pending,
            ),
            RecoveryRecipient::Reserve => (&mut self.reserve_claimable, &mut self.reserve_pending),
        }
    }

    pub(super) fn begin_distribution(&mut self, recipient: RecoveryRecipient) -> NearToken {
        let (claimable, pending) = self.balances(recipient);
        assert!(pending.is_zero(), "Recovery distribution pending");
        assert!(!claimable.is_zero(), "No recovery funds to distribute");
        *pending = std::mem::take(claimable);
        *pending
    }

    pub(super) fn finish_distribution(&mut self, recipient: RecoveryRecipient, success: bool) {
        let (claimable, pending) = self.balances(recipient);
        assert!(!pending.is_zero(), "No pending recovery distribution");
        let amount = std::mem::take(pending);
        if !success {
            *claimable = claimable.saturating_add(amount);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn amount(millinear: u128) -> NearToken {
        NearToken::from_millinear(millinear)
    }

    #[test]
    fn actual_recovery_reduces_payout_before_applying_limit() {
        let mut claim = ClaimRecovery::default();
        claim.record(amount(50), Some(amount(300)));
        assert_eq!(claim.payout(amount(300), super::super::LIMIT), amount(200));
        assert_eq!(claim.beneficiary_claimable, amount(50));

        let mut capped = ClaimRecovery::default();
        capped.record(amount(50), Some(amount(600)));
        assert_eq!(capped.payout(amount(600), super::super::LIMIT), amount(400));
        capped.record(amount(510), Some(amount(600)));
        assert!(capped.payout(amount(600), super::super::LIMIT).is_zero());
    }

    #[test]
    fn post_payout_recovery_follows_waterfall_across_deposits_and_retries() {
        let mut claim = ClaimRecovery::default();
        claim.record(amount(50), Some(amount(600)));
        claim.settle(amount(600), claim.payout(amount(600), super::super::LIMIT));
        assert_eq!(
            claim.begin_distribution(RecoveryRecipient::Beneficiary),
            amount(50)
        );

        claim.record(amount(120), None);
        assert_eq!(claim.beneficiary_over_limit, amount(100));
        assert_eq!(claim.reserve_reimbursement, amount(20));
        assert_eq!(
            claim.begin_distribution(RecoveryRecipient::Reserve),
            amount(20)
        );

        claim.record(amount(430), None);
        assert_eq!(claim.beneficiary_deductible, amount(50));
        assert_eq!(claim.reserve_reimbursement, amount(400));
        claim.finish_distribution(RecoveryRecipient::Beneficiary, true);
        claim.finish_distribution(RecoveryRecipient::Reserve, false);
        assert_eq!(
            claim.begin_distribution(RecoveryRecipient::Beneficiary),
            amount(150)
        );
        assert_eq!(
            claim.begin_distribution(RecoveryRecipient::Reserve),
            amount(400)
        );
        claim.finish_distribution(RecoveryRecipient::Reserve, true);
        assert!(claim.reserve_claimable.is_zero());
        assert_eq!(claim.reserve_reimbursement, amount(400));
    }

    #[test]
    #[should_panic(expected = "Recovery distribution pending")]
    fn duplicate_distribution_cannot_spend_pending_credit() {
        let mut claim = ClaimRecovery::default();
        claim.record(amount(50), Some(amount(300)));
        claim.begin_distribution(RecoveryRecipient::Beneficiary);
        claim.record(amount(50), Some(amount(300)));
        claim.begin_distribution(RecoveryRecipient::Beneficiary);
    }

    #[test]
    #[should_panic(expected = "Recovery exceeds outstanding loss")]
    fn cumulative_recovery_cannot_exceed_claimed_loss() {
        let mut claim = ClaimRecovery::default();
        claim.record(amount(50), Some(amount(300)));
        claim.settle(amount(300), claim.payout(amount(300), super::super::LIMIT));
        claim.record(amount(251), None);
    }
}
