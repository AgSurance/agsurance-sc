use super::*;

const MAX_TRANSFERS: usize = 8;
const MAX_VENDOR_VALIDITY_SECONDS: u64 = 30 * 24 * 60 * 60;

#[near(serializers = [borsh, json])]
#[derive(Clone, PartialEq, Eq)]
pub struct TransferEvidence {
    pub receipt: Evidence,
    pub action_index: u32,
    pub signer_id: AccountId,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct ObservedClaim {
    pub transfers: Vec<TransferEvidence>,
    evidence_hash: String,
    approving: u8,
    rejecting: u8,
    rejection_reason: Option<String>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct VendorConfirmation {
    vendor: AccountId,
    new_supplier: AccountId,
    expires_at: u64,
}

fn is_chain_hash(hash: &str) -> bool {
    (43..=44).contains(&hash.len()) && hash.bytes().all(|byte|
        matches!(byte, b'1'..=b'9' | b'A'..=b'H' | b'J'..=b'N' | b'P'..=b'Z' | b'a'..=b'k' | b'm'..=b'z')
    )
}

impl Policy {
    pub(super) fn observed_claim_limit(
        &self,
        transfers: &[TransferEvidence],
        recovered: NearToken,
    ) -> NearToken {
        let mut cap = self.current_limit();
        let mut previous_limit = self.coverage_limit;
        for change in &self.limit_changes {
            let later_loss = transfers
                .iter()
                .filter(|transfer| transfer.receipt.block_height >= change.effective_height)
                .fold(ZERO, |sum, transfer| {
                    sum.saturating_add(transfer.receipt.amount)
                });
            // Unattributed recoveries reduce the most recent losses first.
            cap = cap.min(previous_limit.saturating_add(later_loss.saturating_sub(recovered)));
            previous_limit = change.limit;
        }
        cap
    }
}

#[near]
impl CoverageVault {
    pub fn confirm_vendor_change(
        &mut self,
        policy_id: u32,
        new_supplier: AccountId,
        expires_at: u64,
    ) {
        let policy = self.policy.as_ref().expect("No policy");
        assert_eq!(policy.id, policy_id, "Wrong policy");
        assert_eq!(
            env::predecessor_account_id(),
            policy.mandate.supplier,
            "Current supplier required"
        );
        assert!(
            new_supplier.as_str().ends_with(".testnet")
                && new_supplier != self.wallet
                && new_supplier != policy.mandate.supplier
                && new_supplier != env::current_account_id(),
            "Invalid replacement supplier"
        );
        let now = Self::now();
        assert!(
            now < expires_at && expires_at - now <= MAX_VENDOR_VALIDITY_SECONDS,
            "Invalid confirmation expiry"
        );
        self.vendor_confirmations.insert(
            policy_id,
            VendorConfirmation {
                vendor: policy.mandate.supplier.clone(),
                new_supplier,
                expires_at,
            },
        );
    }

    pub fn vendor_confirmation(&self, policy_id: u32) -> Option<VendorConfirmation> {
        self.vendor_confirmations.get(&policy_id).cloned()
    }

    pub fn submit_evidence_claim(
        &mut self,
        policy_id: u32,
        transfers: Vec<TransferEvidence>,
    ) -> String {
        self.assert_no_increase();
        let caller = env::predecessor_account_id();
        if caller != self.owner && caller != self.beneficiary {
            self.assert_connected_agent();
        }
        let code_hash = self
            .wallet_code_hash
            .as_ref()
            .expect("Wallet code hash required");
        assert!(is_chain_hash(code_hash), "Invalid wallet code hash");
        assert!(
            Self::now() < self.filing_deadline().expect("No policy"),
            "Claim filing window has closed"
        );
        let policy = self.policy.as_ref().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Policy cannot accept claim"
        );
        assert!(
            (1..=MAX_TRANSFERS).contains(&transfers.len()),
            "Provide one to eight transfers"
        );
        let mut previous: Option<(&str, u32)> = None;
        let mut loss = NearToken::from_yoctonear(0);
        for transfer in &transfers {
            let receipt = &transfer.receipt;
            assert!(
                is_chain_hash(&receipt.receipt_id) && is_chain_hash(&receipt.transaction_hash),
                "Invalid receipt or transaction hash"
            );
            let key = (receipt.receipt_id.as_str(), transfer.action_index);
            assert!(
                previous.is_none_or(|last| last < key),
                "Sort transfers by unique receipt and action index"
            );
            previous = Some(key);
            assert!(
                !self
                    .used_transfer_actions
                    .contains(&(receipt.receipt_id.clone(), transfer.action_index)),
                "Transfer already claimed"
            );
            assert!(
                self.transfer_is_covered(policy, receipt),
                "Transfer is outside paid coverage"
            );
            assert!(
                receipt.recipient != policy.mandate.supplier
                    || receipt.amount > policy.mandate.limit,
                "Transfer matches the mandate"
            );
            assert!(!receipt.amount.is_zero(), "Loss amount required");
            loss = loss.checked_add(receipt.amount).expect("Loss overflow");
        }
        assert!(loss > DEDUCTIBLE, "Loss does not exceed deductible");
        let digest = env::sha256(
            &near_sdk::borsh::to_vec(&(
                env::current_account_id(),
                self.wallet.clone(),
                code_hash,
                policy.id,
                &policy.mandate,
                &transfers,
            ))
            .expect("Evidence encoding"),
        );
        let evidence_hash: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        let claim_limit = policy.observed_claim_limit(&transfers, ZERO);
        let policy = self.policy.as_mut().expect("No policy");
        policy.loss = loss;
        policy.claim_limit = claim_limit;
        policy.payout = loss.saturating_sub(DEDUCTIBLE).min(claim_limit);
        policy.phase = Phase::InReview;
        policy.review_deadline = Some(Self::now() + u64::from(self.review_seconds));
        policy.observed_claim = Some(ObservedClaim {
            transfers,
            evidence_hash: evidence_hash.clone(),
            approving: 0,
            rejecting: 0,
            rejection_reason: None,
        });
        evidence_hash
    }

    pub fn vote_evidence_claim(
        &mut self,
        policy_id: u32,
        evidence_hash: String,
        approve: bool,
        reason: Option<String>,
    ) -> u32 {
        let reviewer = self
            .reviewers
            .iter()
            .position(|id| *id == env::predecessor_account_id())
            .expect("Reviewer required");
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::InReview,
            "Claim is not in review"
        );
        let claim = policy.observed_claim.as_mut().expect("No evidence claim");
        assert_eq!(evidence_hash, claim.evidence_hash, "Evidence hash changed");
        let vote = 1 << reviewer;
        assert!(policy.votes & vote == 0, "Reviewer already voted");
        if approve {
            assert!(reason.is_none(), "Approval has no rejection reason");
            claim.approving |= vote;
        } else {
            let reason = reason
                .filter(|value| !value.trim().is_empty() && value.len() <= 120)
                .expect("Rejection reason required");
            assert!(
                claim
                    .rejection_reason
                    .as_ref()
                    .is_none_or(|prior| prior == &reason),
                "Rejection reasons must match"
            );
            claim.rejection_reason = Some(reason);
            claim.rejecting |= vote;
        }
        policy.votes |= vote;
        if claim.approving.count_ones() == 2 {
            for transfer in &claim.transfers {
                assert!(
                    self.used_transfer_actions
                        .insert((transfer.receipt.receipt_id.clone(), transfer.action_index)),
                    "Transfer already claimed"
                );
            }
            policy.phase = Phase::Approved;
        } else if claim.rejecting.count_ones() == 2 {
            policy.phase = Phase::Rejected;
        }
        policy.votes.count_ones()
    }

    pub(super) fn transfer_is_covered(&self, policy: &Policy, evidence: &Evidence) -> bool {
        let Some(time_ms) = evidence.block_timestamp_ms else {
            return false;
        };
        let time = time_ms / 1000;
        time <= Self::now()
            && evidence.block_height >= policy.activation_height
            && self.retired_at_ms.is_none_or(|retired| time_ms < retired)
            && Self::window_at(policy, self.window_seconds, time)
                .and_then(|window| policy.windows[window])
                .is_some_and(|start| start <= time)
    }
}
