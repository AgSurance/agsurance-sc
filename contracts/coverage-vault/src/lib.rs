use near_sdk::{env, near, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseError};

const PREMIUM: NearToken = NearToken::from_millinear(10);
const LIMIT: NearToken = NearToken::from_millinear(400);
const DEDUCTIBLE: NearToken = NearToken::from_millinear(50);

#[near(serializers = [borsh, json])]
pub struct Incident {
    policy_id: u32,
    amount: NearToken,
    recipient: AccountId,
    block_height: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Active,
    CheckingEvidence,
    InReview,
    Approved,
    Paying,
    Paid,
}

#[near(serializers = [borsh, json])]
#[derive(Clone)]
struct Policy {
    id: u32,
    activation_height: u64,
    phase: Phase,
    votes: u8,
    loss: NearToken,
    payout: NearToken,
}

#[near(serializers = [json])]
pub struct Dashboard {
    reserve: NearToken,
    reserved: NearToken,
    premium: NearToken,
    limit: NearToken,
    deductible: NearToken,
    policy: Option<Policy>,
    beneficiary: AccountId,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct CoverageVault {
    owner: AccountId,
    wallet: AccountId,
    runner: AccountId,
    beneficiary: AccountId,
    reviewers: [AccountId; 3],
    reserve: NearToken,
    policy: Option<Policy>,
}

#[near]
impl CoverageVault {
    #[init]
    pub fn new(
        owner: AccountId,
        wallet: AccountId,
        runner: AccountId,
        beneficiary: AccountId,
        reviewers: [AccountId; 3],
    ) -> Self {
        assert!(env::current_account_id().as_str().ends_with(".testnet"));
        assert!(
            reviewers[0] != reviewers[1]
                && reviewers[0] != reviewers[2]
                && reviewers[1] != reviewers[2],
            "Reviewers must be distinct"
        );
        assert!(
            reviewers
                .iter()
                .all(|r| r != &owner && r != &runner && r != &wallet && r != &beneficiary),
            "Reviewers must have separate roles"
        );
        Self {
            owner,
            wallet,
            runner,
            beneficiary,
            reviewers,
            reserve: NearToken::from_yoctonear(0),
            policy: None,
        }
    }

    pub fn contract_kind(&self) -> &'static str {
        "coverage-vault"
    }

    pub fn dashboard(&self) -> Dashboard {
        let reserved = if self.policy.as_ref().is_some_and(|p| p.phase != Phase::Paid) {
            LIMIT
        } else {
            NearToken::from_yoctonear(0)
        };
        Dashboard {
            reserve: self.reserve,
            reserved,
            premium: PREMIUM,
            limit: LIMIT,
            deductible: DEDUCTIBLE,
            policy: self.policy.clone(),
            beneficiary: self.beneficiary.clone(),
        }
    }

    #[payable]
    pub fn fund_reserve(&mut self) {
        self.reserve = self
            .reserve
            .checked_add(env::attached_deposit())
            .expect("Reserve overflow");
    }

    #[payable]
    pub fn buy_policy(&mut self) -> u32 {
        assert_eq!(env::predecessor_account_id(), self.owner, "Owner required");
        assert!(
            self.policy.as_ref().is_none_or(|p| p.phase == Phase::Paid),
            "Policy already active"
        );
        assert!(env::attached_deposit() == PREMIUM, "Incorrect premium");
        assert!(self.reserve >= LIMIT, "Reserve capacity unavailable");
        let id = self
            .policy
            .as_ref()
            .map_or(1, |p| p.id.checked_add(1).expect("Policy ID overflow"));
        self.policy = Some(Policy {
            id,
            activation_height: env::block_height(),
            phase: Phase::Active,
            votes: 0,
            loss: NearToken::from_yoctonear(0),
            payout: NearToken::from_yoctonear(0),
        });
        id
    }

    pub fn submit_claim(&mut self, policy_id: u32) -> Promise {
        assert_eq!(
            env::predecessor_account_id(),
            self.runner,
            "Runner required"
        );
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Active,
            "Policy cannot accept claim"
        );
        policy.phase = Phase::CheckingEvidence;
        Promise::new(self.wallet.clone())
            .function_call(
                "incident",
                b"{}".to_vec(),
                NearToken::from_yoctonear(0),
                Gas::from_tgas(8),
            )
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(15))
                    .verify_incident(),
            )
    }

    #[private]
    pub fn verify_incident(
        &mut self,
        #[callback_result] result: Result<Option<Incident>, PromiseError>,
    ) -> bool {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.phase == Phase::CheckingEvidence,
            "No pending evidence"
        );
        if let Ok(Some(incident)) = result {
            if incident.policy_id == policy.id
                && incident.block_height >= policy.activation_height
                && incident.amount > DEDUCTIBLE
            {
                policy.loss = incident.amount;
                policy.payout = incident
                    .amount
                    .checked_sub(DEDUCTIBLE)
                    .expect("Deductible")
                    .min(LIMIT);
                policy.phase = Phase::InReview;
                return true;
            }
        }
        policy.phase = Phase::Active;
        false
    }

    pub fn approve_claim(&mut self, policy_id: u32) -> u32 {
        let reviewer = self
            .reviewers
            .iter()
            .position(|r| r == &env::predecessor_account_id())
            .expect("Reviewer required");
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::InReview,
            "Claim is not in review"
        );
        let vote = 1 << reviewer;
        assert!(policy.votes & vote == 0, "Reviewer already voted");
        policy.votes |= vote;
        let count = policy.votes.count_ones();
        if count == 2 {
            policy.phase = Phase::Approved;
        }
        count
    }

    pub fn pay_claim(&mut self, policy_id: u32) -> Promise {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(
            policy.id == policy_id && policy.phase == Phase::Approved,
            "Claim is not payable"
        );
        policy.phase = Phase::Paying;
        Promise::new(self.beneficiary.clone())
            .transfer(policy.payout)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(Gas::from_tgas(14))
                    .finish_payout(),
            )
    }

    #[private]
    pub fn finish_payout(&mut self) -> bool {
        let policy = self.policy.as_mut().expect("No policy");
        assert!(policy.phase == Phase::Paying, "No pending payout");
        if env::promise_result_checked(0, 0).is_err() {
            policy.phase = Phase::Approved;
            return false;
        }
        self.reserve = self
            .reserve
            .checked_sub(policy.payout)
            .expect("Insufficient reserve");
        policy.phase = Phase::Paid;
        true
    }
}
