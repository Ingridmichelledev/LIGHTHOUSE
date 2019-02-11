use crate::test_utils::TestRandom;
use crate::{
    validator::StatusFlags, validator_registry::get_active_validator_indices, AggregatePublicKey,
    Attestation, AttestationData, Bitfield, ChainSpec, Crosslink, Epoch, Eth1Data, Eth1DataVote,
    Fork, Hash256, PendingAttestation, PublicKey, Signature, Slot, Validator,
};
use bls::bls_verify_aggregate;
use honey_badger_split::SplitExt;
use rand::RngCore;
use serde_derive::Serialize;
use ssz::{hash, Decodable, DecodeError, Encodable, SszStream, TreeHash};
use std::collections::HashMap;
use std::ops::Range;
use vec_shuffle::shuffle;

// TODO: define elsehwere.
const DOMAIN_PROPOSAL: u64 = 2;
const DOMAIN_EXIT: u64 = 3;
const DOMAIN_RANDAO: u64 = 4;
const PHASE_0_CUSTODY_BIT: bool = false;
const DOMAIN_ATTESTATION: u64 = 1;

pub enum Error {
    InsufficientValidators,
    BadBlockSignature,
    InvalidEpoch(Slot, Range<Epoch>),
    CommitteesError(CommitteesError),
}

/*
#[derive(Debug, PartialEq)]
pub enum BlockProcessingError {
    DBError(String),
    StateAlreadyTransitioned,
    PresentSlotIsNone,
    UnableToDecodeBlock,
    MissingParentState(Hash256),
    InvalidParentState(Hash256),
    MissingBeaconBlock(Hash256),
    InvalidBeaconBlock(Hash256),
    MissingParentBlock(Hash256),
    NoBlockProducer,
    StateSlotMismatch,
    BadBlockSignature,
    BadRandaoSignature,
    MaxProposerSlashingsExceeded,
    BadProposerSlashing,
    MaxAttestationsExceeded,
    InvalidAttestation(AttestationValidationError),
    NoBlockRoot,
    MaxDepositsExceeded,
    MaxExitsExceeded,
    BadExit,
    BadCustodyReseeds,
    BadCustodyChallenges,
    BadCustodyResponses,
    CommitteesError(CommitteesError),
    SlotProcessingError(SlotProcessingError),
}
*/

/*
#[derive(Debug, PartialEq)]
pub enum EpochError {
    UnableToDetermineProducer,
    NoBlockRoots,
    BaseRewardQuotientIsZero,
    CommitteesError(CommitteesError),
    AttestationParticipantsError(AttestationParticipantsError),
    InclusionError(InclusionError),
    WinningRootError(WinningRootError),
}
*/

#[derive(Debug, PartialEq)]
pub enum WinningRootError {
    NoWinningRoot,
    AttestationParticipantsError(AttestationParticipantsError),
}

#[derive(Debug, PartialEq)]
pub enum CommitteesError {
    InvalidEpoch,
    InsufficientNumberOfValidators,
}

#[derive(Debug, PartialEq)]
pub enum InclusionError {
    NoIncludedAttestations,
    AttestationParticipantsError(AttestationParticipantsError),
}

#[derive(Debug, PartialEq)]
pub enum AttestationParticipantsError {
    NoCommitteeForShard,
    NoCommittees,
    BadBitfieldLength,
    CommitteesError(CommitteesError),
}

/*
#[derive(Debug, PartialEq)]
pub enum SlotProcessingError {
    CommitteesError(CommitteesError),
    EpochProcessingError(EpochError),
}
*/

#[derive(Debug, PartialEq)]
pub enum AttestationValidationError {
    IncludedTooEarly,
    IncludedTooLate,
    WrongJustifiedSlot,
    WrongJustifiedRoot,
    BadLatestCrosslinkRoot,
    BadSignature,
    ShardBlockRootNotZero,
    NoBlockRoot,
    AttestationParticipantsError(AttestationParticipantsError),
}

#[derive(Clone)]
pub struct WinningRoot {
    pub shard_block_root: Hash256,
    pub attesting_validator_indices: Vec<usize>,
    pub total_balance: u64,
    pub total_attesting_balance: u64,
}

macro_rules! ensure {
    ($condition: expr, $result: expr) => {
        if !$condition {
            return Err($result);
        }
    };
}

macro_rules! safe_add_assign {
    ($a: expr, $b: expr) => {
        $a = $a.saturating_add($b);
    };
}
macro_rules! safe_sub_assign {
    ($a: expr, $b: expr) => {
        $a = $a.saturating_sub($b);
    };
}

// Custody will not be added to the specs until Phase 1 (Sharding Phase) so dummy class used.
type CustodyChallenge = usize;

#[derive(Debug, PartialEq, Clone, Default, Serialize)]
pub struct BeaconState {
    // Misc
    pub slot: Slot,
    pub genesis_time: u64,
    pub fork: Fork,

    // Validator registry
    pub validator_registry: Vec<Validator>,
    pub validator_balances: Vec<u64>,
    pub validator_registry_update_epoch: Epoch,

    // Randomness and committees
    pub latest_randao_mixes: Vec<Hash256>,
    pub previous_epoch_start_shard: u64,
    pub current_epoch_start_shard: u64,
    pub previous_calculation_epoch: Epoch,
    pub current_calculation_epoch: Epoch,
    pub previous_epoch_seed: Hash256,
    pub current_epoch_seed: Hash256,

    // Finality
    pub previous_justified_epoch: Epoch,
    pub justified_epoch: Epoch,
    pub justification_bitfield: u64,
    pub finalized_epoch: Epoch,

    // Recent state
    pub latest_crosslinks: Vec<Crosslink>,
    pub latest_block_roots: Vec<Hash256>,
    pub latest_penalized_balances: Vec<u64>,
    pub latest_attestations: Vec<PendingAttestation>,
    pub batched_block_roots: Vec<Hash256>,

    // Ethereum 1.0 chain data
    pub latest_eth1_data: Eth1Data,
    pub eth1_data_votes: Vec<Eth1DataVote>,
}

impl BeaconState {
    pub fn canonical_root(&self) -> Hash256 {
        Hash256::from(&self.hash_tree_root()[..])
    }

    pub fn current_epoch(&self, spec: &ChainSpec) -> Epoch {
        self.slot.epoch(spec.epoch_length)
    }

    pub fn previous_epoch(&self, spec: &ChainSpec) -> Epoch {
        self.current_epoch(spec).saturating_sub(1_u64)
    }

    pub fn next_epoch(&self, spec: &ChainSpec) -> Epoch {
        self.current_epoch(spec).saturating_add(1_u64)
    }

    pub fn current_epoch_start_slot(&self, spec: &ChainSpec) -> Slot {
        self.current_epoch(spec).start_slot(spec.epoch_length)
    }

    pub fn previous_epoch_start_slot(&self, spec: &ChainSpec) -> Slot {
        self.previous_epoch(spec).start_slot(spec.epoch_length)
    }

    /// Return the number of committees in one epoch.
    ///
    /// TODO: this should probably be a method on `ChainSpec`.
    ///
    /// Spec v0.1
    pub fn get_epoch_committee_count(
        &self,
        active_validator_count: usize,
        spec: &ChainSpec,
    ) -> u64 {
        std::cmp::max(
            1,
            std::cmp::min(
                spec.shard_count / spec.epoch_length,
                active_validator_count as u64 / spec.epoch_length / spec.target_committee_size,
            ),
        ) * spec.epoch_length
    }

    /// Shuffle ``validators`` into crosslink committees seeded by ``seed`` and ``epoch``.
    /// Return a list of ``committees_per_epoch`` committees where each
    /// committee is itself a list of validator indices.
    ///
    /// Spec v0.1
    pub fn get_shuffling(&self, seed: Hash256, epoch: Epoch, spec: &ChainSpec) -> Vec<Vec<usize>> {
        let active_validator_indices =
            get_active_validator_indices(&self.validator_registry, epoch);

        let committees_per_epoch =
            self.get_epoch_committee_count(active_validator_indices.len(), spec);

        // TODO: check that Hash256::from(u64) matches 'int_to_bytes32'.
        let seed = seed ^ Hash256::from(epoch.as_u64());
        // TODO: fix `expect` assert.
        let shuffled_active_validator_indices =
            shuffle(&seed, active_validator_indices).expect("Max validator count exceed!");

        shuffled_active_validator_indices
            .honey_badger_split(committees_per_epoch as usize)
            .filter_map(|slice: &[usize]| Some(slice.to_vec()))
            .collect()
    }

    /// Return the number of committees in the previous epoch.
    ///
    /// Spec v0.1
    fn get_previous_epoch_committee_count(&self, spec: &ChainSpec) -> u64 {
        let previous_active_validators =
            get_active_validator_indices(&self.validator_registry, self.previous_calculation_epoch);
        self.get_epoch_committee_count(previous_active_validators.len(), spec)
    }

    /// Return the number of committees in the current epoch.
    ///
    /// Spec v0.1
    pub fn get_current_epoch_committee_count(&self, spec: &ChainSpec) -> u64 {
        let current_active_validators =
            get_active_validator_indices(&self.validator_registry, self.current_calculation_epoch);
        self.get_epoch_committee_count(current_active_validators.len(), spec)
    }

    /// Return the number of committees in the next epoch.
    ///
    /// Spec v0.1
    pub fn get_next_epoch_committee_count(&self, spec: &ChainSpec) -> u64 {
        let current_active_validators =
            get_active_validator_indices(&self.validator_registry, self.current_epoch(spec) + 1);
        self.get_epoch_committee_count(current_active_validators.len(), spec)
    }

    /// Return the list of ``(committee, shard)`` tuples for the ``slot``.
    ///
    /// Note: There are two possible shufflings for crosslink committees for a
    /// `slot` in the next epoch: with and without a `registry_change`
    ///
    /// Spec v0.1
    pub fn get_crosslink_committees_at_slot(
        &self,
        slot: Slot,
        registry_change: bool,
        spec: &ChainSpec,
    ) -> Result<Vec<(Vec<usize>, u64)>, CommitteesError> {
        let epoch = slot.epoch(spec.epoch_length);
        let current_epoch = self.current_epoch(spec);
        let previous_epoch = if current_epoch == spec.genesis_epoch {
            current_epoch
        } else {
            current_epoch.saturating_sub(1_u64)
        };
        let next_epoch = current_epoch + 1;

        ensure!(
            (previous_epoch <= epoch) & (epoch < next_epoch),
            CommitteesError::InvalidEpoch
        );

        let offset = slot.as_u64() % spec.epoch_length;

        let (committees_per_slot, shuffling, slot_start_shard) = if epoch < current_epoch {
            let committees_per_slot = self.get_previous_epoch_committee_count(spec);
            let shuffling = self.get_shuffling(
                self.previous_epoch_seed,
                self.previous_calculation_epoch,
                spec,
            );
            let slot_start_shard =
                (self.previous_epoch_start_shard + committees_per_slot * offset) % spec.shard_count;
            (committees_per_slot, shuffling, slot_start_shard)
        } else {
            let committees_per_slot = self.get_current_epoch_committee_count(spec);
            let shuffling = self.get_shuffling(
                self.current_epoch_seed,
                self.current_calculation_epoch,
                spec,
            );
            let slot_start_shard =
                (self.current_epoch_start_shard + committees_per_slot * offset) % spec.shard_count;
            (committees_per_slot, shuffling, slot_start_shard)
        };

        let mut crosslinks_at_slot = vec![];
        for i in 0..committees_per_slot {
            let tuple = (
                shuffling[(committees_per_slot * offset + i) as usize].clone(),
                (slot_start_shard + i) % spec.shard_count,
            );
            crosslinks_at_slot.push(tuple)
        }
        Ok(crosslinks_at_slot)
    }

    pub fn attestation_slot_and_shard_for_validator(
        &self,
        validator_index: usize,
        spec: &ChainSpec,
    ) -> Result<Option<(Slot, u64, u64)>, CommitteesError> {
        let mut result = None;
        for slot in self.current_epoch(spec).slot_iter(spec.epoch_length) {
            for (committee, shard) in self.get_crosslink_committees_at_slot(slot, false, spec)? {
                if let Some(committee_index) = committee.iter().position(|&i| i == validator_index)
                {
                    result = Some((slot, shard, committee_index as u64));
                }
            }
        }
        Ok(result)
    }

    pub fn get_entry_exit_effect_epoch(&self, epoch: Epoch, spec: &ChainSpec) -> Epoch {
        epoch + 1 + spec.entry_exit_delay
    }

    /// Returns the beacon proposer index for the `slot`.
    /// If the state does not contain an index for a beacon proposer at the requested `slot`, then `None` is returned.
    pub fn get_beacon_proposer_index(
        &self,
        slot: Slot,
        spec: &ChainSpec,
    ) -> Result<usize, CommitteesError> {
        let committees = self.get_crosslink_committees_at_slot(slot, false, spec)?;
        committees
            .first()
            .ok_or(CommitteesError::InsufficientNumberOfValidators)
            .and_then(|(first_committee, _)| {
                let index = (slot.as_usize())
                    .checked_rem(first_committee.len())
                    .ok_or(CommitteesError::InsufficientNumberOfValidators)?;
                // NOTE: next index will not panic as we have already returned if this is the case
                Ok(first_committee[index])
            })
    }

    /// Process the penalties and prepare the validators who are eligible to withdrawal.
    ///
    /// Spec v0.2.0
    fn process_penalties_and_exits(&mut self, spec: &ChainSpec) {
        let current_epoch = self.current_epoch(spec);
        let active_validator_indices =
            get_active_validator_indices(&self.validator_registry, current_epoch);
        let total_balance = self.get_total_balance(&active_validator_indices[..], spec);

        for index in 0..self.validator_balances.len() {
            let validator = &self.validator_registry[index];

            if current_epoch
                == validator.penalized_epoch + Epoch::from(spec.latest_penalized_exit_length / 2)
            {
                let epoch_index: usize =
                    current_epoch.as_usize() % spec.latest_penalized_exit_length;

                let total_at_start = self.latest_penalized_balances
                    [(epoch_index + 1) % spec.latest_penalized_exit_length];
                let total_at_end = self.latest_penalized_balances[epoch_index];
                let total_penalities = total_at_end.saturating_sub(total_at_start);
                let penalty = self.get_effective_balance(index, spec)
                    * std::cmp::min(total_penalities * 3, total_balance)
                    / total_balance;
                safe_sub_assign!(self.validator_balances[index], penalty);
            }
        }

        let eligible = |index: usize| {
            let validator = &self.validator_registry[index];

            if validator.penalized_epoch <= current_epoch {
                let penalized_withdrawal_epochs = spec.latest_penalized_exit_length / 2;
                current_epoch >= validator.penalized_epoch + penalized_withdrawal_epochs as u64
            } else {
                current_epoch >= validator.exit_epoch + spec.min_validator_withdrawal_epochs
            }
        };

        let mut eligable_indices: Vec<usize> = (0..self.validator_registry.len())
            .filter(|i| eligible(*i))
            .collect();
        eligable_indices.sort_by_key(|i| self.validator_registry[*i].exit_epoch);
        let mut withdrawn_so_far = 0;
        for index in eligable_indices {
            self.prepare_validator_for_withdrawal(index);
            withdrawn_so_far += 1;
            if withdrawn_so_far >= spec.max_withdrawals_per_epoch {
                break;
            }
        }
    }

    /// Return the randao mix at a recent ``epoch``.
    ///
    /// Returns `None` if the epoch is out-of-bounds of `self.latest_randao_mixes`.
    ///
    /// Spec v0.2.0
    fn get_randao_mix(&mut self, epoch: Epoch, spec: &ChainSpec) -> Option<&Hash256> {
        self.latest_randao_mixes
            .get(epoch.as_usize() % spec.latest_randao_mixes_length)
    }

    /// Update validator registry, activating/exiting validators if possible.
    ///
    /// Spec v0.2.0
    fn update_validator_registry(&mut self, spec: &ChainSpec) {
        let current_epoch = self.current_epoch(spec);
        let active_validator_indices =
            get_active_validator_indices(&self.validator_registry, current_epoch);
        let total_balance = self.get_total_balance(&active_validator_indices[..], spec);

        let max_balance_churn = std::cmp::max(
            spec.max_deposit_amount,
            total_balance / (2 * spec.max_balance_churn_quotient),
        );

        let mut balance_churn = 0;
        for index in 0..self.validator_registry.len() {
            let validator = &self.validator_registry[index];

            if (validator.activation_epoch > self.get_entry_exit_effect_epoch(current_epoch, spec))
                && self.validator_balances[index] >= spec.max_deposit_amount
            {
                balance_churn += self.get_effective_balance(index, spec);
                if balance_churn > max_balance_churn {
                    break;
                }
                self.activate_validator(index, false, spec);
            }
        }

        let mut balance_churn = 0;
        for index in 0..self.validator_registry.len() {
            let validator = &self.validator_registry[index];

            if (validator.exit_epoch > self.get_entry_exit_effect_epoch(current_epoch, spec))
                && validator.status_flags == Some(StatusFlags::InitiatedExit)
            {
                balance_churn += self.get_effective_balance(index, spec);
                if balance_churn > max_balance_churn {
                    break;
                }

                self.exit_validator(index, spec);
            }
        }

        self.validator_registry_update_epoch = current_epoch;
    }

    /// Activate the validator of the given ``index``.
    ///
    /// Spec v0.2.0
    fn activate_validator(&mut self, validator_index: usize, is_genesis: bool, spec: &ChainSpec) {
        let current_epoch = self.current_epoch(spec);

        self.validator_registry[validator_index].activation_epoch = if is_genesis {
            spec.genesis_epoch
        } else {
            self.get_entry_exit_effect_epoch(current_epoch, spec)
        }
    }

    /// Initiate an exit for the validator of the given `index`.
    ///
    /// Spec v0.2.0
    fn initiate_validator_exit(&mut self, validator_index: usize, spec: &ChainSpec) {
        // TODO: the spec does an `|=` here, ensure this isn't buggy.
        self.validator_registry[validator_index].status_flags = Some(StatusFlags::InitiatedExit);
    }

    /// Exit the validator of the given `index`.
    ///
    /// Spec v0.2.0
    fn exit_validator(&mut self, validator_index: usize, spec: &ChainSpec) {
        let current_epoch = self.current_epoch(spec);

        if self.validator_registry[validator_index].exit_epoch
            <= self.get_entry_exit_effect_epoch(current_epoch, spec)
        {
            return;
        }

        self.validator_registry[validator_index].exit_epoch =
            self.get_entry_exit_effect_epoch(current_epoch, spec);
    }

    ///  Penalize the validator of the given ``index``.
    ///
    ///  Exits the validator and assigns its effective balance to the block producer for this
    ///  state.
    ///
    /// Spec v0.2.0
    pub fn penalize_validator(
        &mut self,
        validator_index: usize,
        spec: &ChainSpec,
    ) -> Result<(), CommitteesError> {
        self.exit_validator(validator_index, spec);
        let current_epoch = self.current_epoch(spec);

        self.latest_penalized_balances
            [current_epoch.as_usize() % spec.latest_penalized_exit_length] +=
            self.get_effective_balance(validator_index, spec);

        let whistleblower_index = self.get_beacon_proposer_index(self.slot, spec)?;
        let whistleblower_reward = self.get_effective_balance(validator_index, spec);
        safe_add_assign!(
            self.validator_balances[whistleblower_index as usize],
            whistleblower_reward
        );
        safe_sub_assign!(
            self.validator_balances[validator_index],
            whistleblower_reward
        );
        self.validator_registry[validator_index].penalized_epoch = current_epoch;
        Ok(())
    }

    /// Initiate an exit for the validator of the given `index`.
    ///
    /// Spec v0.2.0
    fn prepare_validator_for_withdrawal(&mut self, validator_index: usize) {
        //TODO: we're not ANDing here, we're setting. Potentially wrong.
        self.validator_registry[validator_index].status_flags = Some(StatusFlags::Withdrawable);
    }

    /// Iterate through the validator registry and eject active validators with balance below
    /// ``EJECTION_BALANCE``.
    ///
    /// Spec v0.2.0
    fn process_ejections(&mut self, spec: &ChainSpec) {
        for validator_index in
            get_active_validator_indices(&self.validator_registry, self.current_epoch(spec))
        {
            if self.validator_balances[validator_index] < spec.ejection_balance {
                self.exit_validator(validator_index, spec)
            }
        }
    }

    /// Returns the penality that should be applied to some validator for inactivity.
    ///
    /// Note: this is defined "inline" in the spec, not as a helper function.
    ///
    /// Spec v0.2.0
    fn inactivity_penalty(
        &self,
        validator_index: usize,
        epochs_since_finality: u64,
        base_reward_quotient: u64,
        spec: &ChainSpec,
    ) -> u64 {
        let effective_balance = self.get_effective_balance(validator_index, spec);
        self.base_reward(validator_index, base_reward_quotient, spec)
            + effective_balance * epochs_since_finality / spec.inactivity_penalty_quotient / 2
    }

    /// Returns the distance between the first included attestation for some validator and this
    /// slot.
    ///
    /// Note: In the spec this is defined "inline", not as a helper function.
    ///
    /// Spec v0.2.0
    fn inclusion_distance(
        &self,
        attestations: &[&PendingAttestation],
        validator_index: usize,
        spec: &ChainSpec,
    ) -> Result<u64, InclusionError> {
        let attestation =
            self.earliest_included_attestation(attestations, validator_index, spec)?;
        Ok((attestation.inclusion_slot - attestation.data.slot).as_u64())
    }

    /// Returns the slot of the earliest included attestation for some validator.
    ///
    /// Note: In the spec this is defined "inline", not as a helper function.
    ///
    /// Spec v0.2.0
    fn inclusion_slot(
        &self,
        attestations: &[&PendingAttestation],
        validator_index: usize,
        spec: &ChainSpec,
    ) -> Result<Slot, InclusionError> {
        let attestation =
            self.earliest_included_attestation(attestations, validator_index, spec)?;
        Ok(attestation.inclusion_slot)
    }

    /// Finds the earliest included attestation for some validator.
    ///
    /// Note: In the spec this is defined "inline", not as a helper function.
    ///
    /// Spec v0.2.0
    fn earliest_included_attestation(
        &self,
        attestations: &[&PendingAttestation],
        validator_index: usize,
        spec: &ChainSpec,
    ) -> Result<PendingAttestation, InclusionError> {
        let mut included_attestations = vec![];

        for (i, a) in attestations.iter().enumerate() {
            let participants =
                self.get_attestation_participants(&a.data, &a.aggregation_bitfield, spec)?;
            if participants
                .iter()
                .find(|i| **i == validator_index)
                .is_some()
            {
                included_attestations.push(i);
            }
        }

        let earliest_attestation_index = included_attestations
            .iter()
            .min_by_key(|i| attestations[**i].inclusion_slot)
            .ok_or_else(|| InclusionError::NoIncludedAttestations)?;
        Ok(attestations[*earliest_attestation_index].clone())
    }

    /// Returns the base reward for some validator.
    ///
    /// Note: In the spec this is defined "inline", not as a helper function.
    ///
    /// Spec v0.2.0
    fn base_reward(
        &self,
        validator_index: usize,
        base_reward_quotient: u64,
        spec: &ChainSpec,
    ) -> u64 {
        self.get_effective_balance(validator_index, spec) / base_reward_quotient / 5
    }

    /// Return the combined effective balance of an array of validators.
    ///
    /// Spec v0.2.0
    pub fn get_total_balance(&self, validator_indices: &[usize], spec: &ChainSpec) -> u64 {
        validator_indices
            .iter()
            .fold(0, |acc, i| acc + self.get_effective_balance(*i, spec))
    }

    /// Return the effective balance (also known as "balance at stake") for a validator with the given ``index``.
    ///
    /// Spec v0.2.0
    pub fn get_effective_balance(&self, validator_index: usize, spec: &ChainSpec) -> u64 {
        std::cmp::min(
            self.validator_balances[validator_index],
            spec.max_deposit_amount,
        )
    }

    /// Return the block root at a recent `slot`.
    ///
    /// Spec v0.2.0
    pub fn get_block_root(&self, slot: Slot, spec: &ChainSpec) -> Option<&Hash256> {
        self.latest_block_roots
            .get(slot.as_usize() % spec.latest_block_roots_length)
    }

    pub(crate) fn winning_root(
        &self,
        shard: u64,
        current_epoch_attestations: &[&PendingAttestation],
        previous_epoch_attestations: &[&PendingAttestation],
        spec: &ChainSpec,
    ) -> Result<WinningRoot, WinningRootError> {
        let mut attestations = current_epoch_attestations.to_vec();
        attestations.append(&mut previous_epoch_attestations.to_vec());

        let mut candidates: HashMap<Hash256, WinningRoot> = HashMap::new();

        let mut highest_seen_balance = 0;

        for a in &attestations {
            if a.data.shard != shard {
                continue;
            }

            let shard_block_root = &a.data.shard_block_root;

            if candidates.contains_key(shard_block_root) {
                continue;
            }

            // TODO: `cargo fmt` makes this rather ugly; tidy up.
            let attesting_validator_indices = attestations.iter().try_fold::<_, _, Result<
                _,
                AttestationParticipantsError,
            >>(
                vec![],
                |mut acc, a| {
                    if (a.data.shard == shard) && (a.data.shard_block_root == *shard_block_root) {
                        acc.append(&mut self.get_attestation_participants(
                            &a.data,
                            &a.aggregation_bitfield,
                            spec,
                        )?);
                    }
                    Ok(acc)
                },
            )?;

            let total_balance: u64 = attesting_validator_indices
                .iter()
                .fold(0, |acc, i| acc + self.get_effective_balance(*i, spec));

            let total_attesting_balance: u64 = attesting_validator_indices
                .iter()
                .fold(0, |acc, i| acc + self.get_effective_balance(*i, spec));

            if total_attesting_balance > highest_seen_balance {
                highest_seen_balance = total_attesting_balance;
            }

            let candidate_root = WinningRoot {
                shard_block_root: shard_block_root.clone(),
                attesting_validator_indices,
                total_attesting_balance,
                total_balance,
            };

            candidates.insert(*shard_block_root, candidate_root);
        }

        Ok(candidates
            .iter()
            .filter_map(|(_hash, candidate)| {
                if candidate.total_attesting_balance == highest_seen_balance {
                    Some(candidate)
                } else {
                    None
                }
            })
            .min_by_key(|candidate| candidate.shard_block_root)
            .ok_or_else(|| WinningRootError::NoWinningRoot)?
            // TODO: avoid clone.
            .clone())
    }

    pub fn get_attestation_participants_union(
        &self,
        attestations: &[&PendingAttestation],
        spec: &ChainSpec,
    ) -> Result<Vec<usize>, AttestationParticipantsError> {
        let mut all_participants = attestations.iter().try_fold::<_, _, Result<
            Vec<usize>,
            AttestationParticipantsError,
        >>(vec![], |mut acc, a| {
            acc.append(&mut self.get_attestation_participants(
                &a.data,
                &a.aggregation_bitfield,
                spec,
            )?);
            Ok(acc)
        })?;
        all_participants.sort_unstable();
        all_participants.dedup();
        Ok(all_participants)
    }

    // TODO: analyse for efficiency improvments. This implementation is naive.
    pub fn get_attestation_participants(
        &self,
        attestation_data: &AttestationData,
        aggregation_bitfield: &Bitfield,
        spec: &ChainSpec,
    ) -> Result<Vec<usize>, AttestationParticipantsError> {
        let crosslink_committees =
            self.get_crosslink_committees_at_slot(attestation_data.slot, false, spec)?;

        let committee_index: usize = crosslink_committees
            .iter()
            .position(|(_committee, shard)| *shard == attestation_data.shard)
            .ok_or_else(|| AttestationParticipantsError::NoCommitteeForShard)?;
        let (crosslink_committee, _shard) = &crosslink_committees[committee_index];

        /*
         * TODO: that bitfield length is valid.
         *
         */

        let mut participants = vec![];
        for (i, validator_index) in crosslink_committee.iter().enumerate() {
            if aggregation_bitfield.get(i).unwrap() {
                participants.push(*validator_index);
            }
        }
        Ok(participants)
    }

    pub fn validate_attestation(
        &self,
        attestation: &Attestation,
        spec: &ChainSpec,
    ) -> Result<(), AttestationValidationError> {
        self.validate_attestation_signature_optional(attestation, spec, true)
    }

    pub fn validate_attestation_without_signature(
        &self,
        attestation: &Attestation,
        spec: &ChainSpec,
    ) -> Result<(), AttestationValidationError> {
        self.validate_attestation_signature_optional(attestation, spec, false)
    }

    fn validate_attestation_signature_optional(
        &self,
        attestation: &Attestation,
        spec: &ChainSpec,
        verify_signature: bool,
    ) -> Result<(), AttestationValidationError> {
        ensure!(
            attestation.data.slot + spec.min_attestation_inclusion_delay <= self.slot,
            AttestationValidationError::IncludedTooEarly
        );
        ensure!(
            attestation.data.slot + spec.epoch_length >= self.slot,
            AttestationValidationError::IncludedTooLate
        );
        if attestation.data.slot >= self.current_epoch_start_slot(spec) {
            ensure!(
                attestation.data.justified_epoch == self.justified_epoch,
                AttestationValidationError::WrongJustifiedSlot
            );
        } else {
            ensure!(
                attestation.data.justified_epoch == self.previous_justified_epoch,
                AttestationValidationError::WrongJustifiedSlot
            );
        }
        ensure!(
            attestation.data.justified_block_root
                == *self
                    .get_block_root(
                        attestation
                            .data
                            .justified_epoch
                            .start_slot(spec.epoch_length),
                        &spec
                    )
                    .ok_or(AttestationValidationError::NoBlockRoot)?,
            AttestationValidationError::WrongJustifiedRoot
        );
        ensure!(
            (attestation.data.latest_crosslink
                == self.latest_crosslinks[attestation.data.shard as usize])
                || (attestation.data.latest_crosslink
                    == self.latest_crosslinks[attestation.data.shard as usize]),
            AttestationValidationError::BadLatestCrosslinkRoot
        );
        if verify_signature {
            let participants = self.get_attestation_participants(
                &attestation.data,
                &attestation.aggregation_bitfield,
                spec,
            )?;
            let mut group_public_key = AggregatePublicKey::new();
            for participant in participants {
                group_public_key.add(
                    self.validator_registry[participant as usize]
                        .pubkey
                        .as_raw(),
                )
            }
            ensure!(
                bls_verify_aggregate(
                    &group_public_key,
                    &attestation.signable_message(PHASE_0_CUSTODY_BIT),
                    &attestation.aggregate_signature,
                    get_domain(
                        &self.fork,
                        attestation.data.slot.epoch(spec.epoch_length),
                        DOMAIN_ATTESTATION
                    )
                ),
                AttestationValidationError::BadSignature
            );
        }
        ensure!(
            attestation.data.shard_block_root == spec.zero_hash,
            AttestationValidationError::ShardBlockRootNotZero
        );
        Ok(())
    }
}

fn merkle_root(_input: &[Hash256]) -> Hash256 {
    Hash256::zero()
}

fn get_domain(_fork: &Fork, _epoch: Epoch, _domain_type: u64) -> u64 {
    // TODO: stubbed out.
    0
}

fn bls_verify(pubkey: &PublicKey, message: &[u8], signature: &Signature, _domain: u64) -> bool {
    // TODO: add domain
    signature.verify(message, pubkey)
}

impl From<AttestationParticipantsError> for AttestationValidationError {
    fn from(e: AttestationParticipantsError) -> AttestationValidationError {
        AttestationValidationError::AttestationParticipantsError(e)
    }
}

impl From<AttestationParticipantsError> for WinningRootError {
    fn from(e: AttestationParticipantsError) -> WinningRootError {
        WinningRootError::AttestationParticipantsError(e)
    }
}

impl From<CommitteesError> for AttestationParticipantsError {
    fn from(e: CommitteesError) -> AttestationParticipantsError {
        AttestationParticipantsError::CommitteesError(e)
    }
}

/*
impl From<AttestationValidationError> for BlockProcessingError {
    fn from(e: AttestationValidationError) -> BlockProcessingError {
        BlockProcessingError::InvalidAttestation(e)
    }
}

impl From<CommitteesError> for BlockProcessingError {
    fn from(e: CommitteesError) -> BlockProcessingError {
        BlockProcessingError::CommitteesError(e)
    }
}

impl From<SlotProcessingError> for BlockProcessingError {
    fn from(e: SlotProcessingError) -> BlockProcessingError {
        BlockProcessingError::SlotProcessingError(e)
    }
}

impl From<CommitteesError> for SlotProcessingError {
    fn from(e: CommitteesError) -> SlotProcessingError {
        SlotProcessingError::CommitteesError(e)
    }
}

impl From<EpochError> for SlotProcessingError {
    fn from(e: EpochError) -> SlotProcessingError {
        SlotProcessingError::EpochProcessingError(e)
    }
}
*/

impl From<AttestationParticipantsError> for InclusionError {
    fn from(e: AttestationParticipantsError) -> InclusionError {
        InclusionError::AttestationParticipantsError(e)
    }
}

/*
impl From<InclusionError> for EpochError {
    fn from(e: InclusionError) -> EpochError {
        EpochError::InclusionError(e)
    }
}

impl From<CommitteesError> for EpochError {
    fn from(e: CommitteesError) -> EpochError {
        EpochError::CommitteesError(e)
    }
}

impl From<AttestationParticipantsError> for EpochError {
    fn from(e: AttestationParticipantsError) -> EpochError {
        EpochError::AttestationParticipantsError(e)
    }
}
*/

impl From<CommitteesError> for Error {
    fn from(e: CommitteesError) -> Error {
        Error::CommitteesError(e)
    }
}

impl Encodable for BeaconState {
    fn ssz_append(&self, s: &mut SszStream) {
        s.append(&self.slot);
        s.append(&self.genesis_time);
        s.append(&self.fork);
        s.append(&self.validator_registry);
        s.append(&self.validator_balances);
        s.append(&self.validator_registry_update_epoch);
        s.append(&self.latest_randao_mixes);
        s.append(&self.previous_epoch_start_shard);
        s.append(&self.current_epoch_start_shard);
        s.append(&self.previous_calculation_epoch);
        s.append(&self.current_calculation_epoch);
        s.append(&self.previous_epoch_seed);
        s.append(&self.current_epoch_seed);
        s.append(&self.previous_justified_epoch);
        s.append(&self.justified_epoch);
        s.append(&self.justification_bitfield);
        s.append(&self.finalized_epoch);
        s.append(&self.latest_crosslinks);
        s.append(&self.latest_block_roots);
        s.append(&self.latest_penalized_balances);
        s.append(&self.latest_attestations);
        s.append(&self.batched_block_roots);
        s.append(&self.latest_eth1_data);
        s.append(&self.eth1_data_votes);
    }
}

impl Decodable for BeaconState {
    fn ssz_decode(bytes: &[u8], i: usize) -> Result<(Self, usize), DecodeError> {
        let (slot, i) = <_>::ssz_decode(bytes, i)?;
        let (genesis_time, i) = <_>::ssz_decode(bytes, i)?;
        let (fork, i) = <_>::ssz_decode(bytes, i)?;
        let (validator_registry, i) = <_>::ssz_decode(bytes, i)?;
        let (validator_balances, i) = <_>::ssz_decode(bytes, i)?;
        let (validator_registry_update_epoch, i) = <_>::ssz_decode(bytes, i)?;
        let (latest_randao_mixes, i) = <_>::ssz_decode(bytes, i)?;
        let (previous_epoch_start_shard, i) = <_>::ssz_decode(bytes, i)?;
        let (current_epoch_start_shard, i) = <_>::ssz_decode(bytes, i)?;
        let (previous_calculation_epoch, i) = <_>::ssz_decode(bytes, i)?;
        let (current_calculation_epoch, i) = <_>::ssz_decode(bytes, i)?;
        let (previous_epoch_seed, i) = <_>::ssz_decode(bytes, i)?;
        let (current_epoch_seed, i) = <_>::ssz_decode(bytes, i)?;
        let (previous_justified_epoch, i) = <_>::ssz_decode(bytes, i)?;
        let (justified_epoch, i) = <_>::ssz_decode(bytes, i)?;
        let (justification_bitfield, i) = <_>::ssz_decode(bytes, i)?;
        let (finalized_epoch, i) = <_>::ssz_decode(bytes, i)?;
        let (latest_crosslinks, i) = <_>::ssz_decode(bytes, i)?;
        let (latest_block_roots, i) = <_>::ssz_decode(bytes, i)?;
        let (latest_penalized_balances, i) = <_>::ssz_decode(bytes, i)?;
        let (latest_attestations, i) = <_>::ssz_decode(bytes, i)?;
        let (batched_block_roots, i) = <_>::ssz_decode(bytes, i)?;
        let (latest_eth1_data, i) = <_>::ssz_decode(bytes, i)?;
        let (eth1_data_votes, i) = <_>::ssz_decode(bytes, i)?;

        Ok((
            Self {
                slot,
                genesis_time,
                fork,
                validator_registry,
                validator_balances,
                validator_registry_update_epoch,
                latest_randao_mixes,
                previous_epoch_start_shard,
                current_epoch_start_shard,
                previous_calculation_epoch,
                current_calculation_epoch,
                previous_epoch_seed,
                current_epoch_seed,
                previous_justified_epoch,
                justified_epoch,
                justification_bitfield,
                finalized_epoch,
                latest_crosslinks,
                latest_block_roots,
                latest_penalized_balances,
                latest_attestations,
                batched_block_roots,
                latest_eth1_data,
                eth1_data_votes,
            },
            i,
        ))
    }
}

impl TreeHash for BeaconState {
    fn hash_tree_root(&self) -> Vec<u8> {
        let mut result: Vec<u8> = vec![];
        result.append(&mut self.slot.hash_tree_root());
        result.append(&mut self.genesis_time.hash_tree_root());
        result.append(&mut self.fork.hash_tree_root());
        result.append(&mut self.validator_registry.hash_tree_root());
        result.append(&mut self.validator_balances.hash_tree_root());
        result.append(&mut self.validator_registry_update_epoch.hash_tree_root());
        result.append(&mut self.latest_randao_mixes.hash_tree_root());
        result.append(&mut self.previous_epoch_start_shard.hash_tree_root());
        result.append(&mut self.current_epoch_start_shard.hash_tree_root());
        result.append(&mut self.previous_calculation_epoch.hash_tree_root());
        result.append(&mut self.current_calculation_epoch.hash_tree_root());
        result.append(&mut self.previous_epoch_seed.hash_tree_root());
        result.append(&mut self.current_epoch_seed.hash_tree_root());
        result.append(&mut self.previous_justified_epoch.hash_tree_root());
        result.append(&mut self.justified_epoch.hash_tree_root());
        result.append(&mut self.justification_bitfield.hash_tree_root());
        result.append(&mut self.finalized_epoch.hash_tree_root());
        result.append(&mut self.latest_crosslinks.hash_tree_root());
        result.append(&mut self.latest_block_roots.hash_tree_root());
        result.append(&mut self.latest_penalized_balances.hash_tree_root());
        result.append(&mut self.latest_attestations.hash_tree_root());
        result.append(&mut self.batched_block_roots.hash_tree_root());
        result.append(&mut self.latest_eth1_data.hash_tree_root());
        result.append(&mut self.eth1_data_votes.hash_tree_root());
        hash(&result)
    }
}

impl<T: RngCore> TestRandom<T> for BeaconState {
    fn random_for_test(rng: &mut T) -> Self {
        Self {
            slot: <_>::random_for_test(rng),
            genesis_time: <_>::random_for_test(rng),
            fork: <_>::random_for_test(rng),
            validator_registry: <_>::random_for_test(rng),
            validator_balances: <_>::random_for_test(rng),
            validator_registry_update_epoch: <_>::random_for_test(rng),
            latest_randao_mixes: <_>::random_for_test(rng),
            previous_epoch_start_shard: <_>::random_for_test(rng),
            current_epoch_start_shard: <_>::random_for_test(rng),
            previous_calculation_epoch: <_>::random_for_test(rng),
            current_calculation_epoch: <_>::random_for_test(rng),
            previous_epoch_seed: <_>::random_for_test(rng),
            current_epoch_seed: <_>::random_for_test(rng),
            previous_justified_epoch: <_>::random_for_test(rng),
            justified_epoch: <_>::random_for_test(rng),
            justification_bitfield: <_>::random_for_test(rng),
            finalized_epoch: <_>::random_for_test(rng),
            latest_crosslinks: <_>::random_for_test(rng),
            latest_block_roots: <_>::random_for_test(rng),
            latest_penalized_balances: <_>::random_for_test(rng),
            latest_attestations: <_>::random_for_test(rng),
            batched_block_roots: <_>::random_for_test(rng),
            latest_eth1_data: <_>::random_for_test(rng),
            eth1_data_votes: <_>::random_for_test(rng),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{SeedableRng, TestRandom, XorShiftRng};
    use ssz::ssz_encode;

    #[test]
    pub fn test_ssz_round_trip() {
        let mut rng = XorShiftRng::from_seed([42; 16]);
        let original = BeaconState::random_for_test(&mut rng);

        let bytes = ssz_encode(&original);
        let (decoded, _) = <_>::ssz_decode(&bytes, 0).unwrap();

        assert_eq!(original, decoded);
    }

    #[test]
    pub fn test_hash_tree_root() {
        let mut rng = XorShiftRng::from_seed([42; 16]);
        let original = BeaconState::random_for_test(&mut rng);

        let result = original.hash_tree_root();

        assert_eq!(result.len(), 32);
        // TODO: Add further tests
        // https://github.com/sigp/lighthouse/issues/170
    }
}
