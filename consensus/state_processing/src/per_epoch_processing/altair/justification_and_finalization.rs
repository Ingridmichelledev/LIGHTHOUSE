use super::ParticipationCache;
use crate::per_epoch_processing::Error;
use crate::per_epoch_processing::{
    weigh_justification_and_finalization, JustificationAndFinalizationState,
};
use safe_arith::SafeArith;
use types::consts::altair::TIMELY_TARGET_FLAG_INDEX;
use types::{BeaconState, EthSpec};

/// Update the justified and finalized checkpoints for matching target attestations.
pub fn process_justification_and_finalization<T: EthSpec>(
    state: &BeaconState<T>,
    participation_cache: &ParticipationCache,
) -> Result<JustificationAndFinalizationState<T>, Error> {
    let justification_and_finalization_state = JustificationAndFinalizationState::new(state);

    if state.current_epoch() <= T::genesis_epoch().safe_add(1)? {
        return Ok(justification_and_finalization_state);
    }

    let previous_epoch = state.previous_epoch();
    let current_epoch = state.current_epoch();
    let previous_indices = participation_cache
        .get_unslashed_participating_indices(TIMELY_TARGET_FLAG_INDEX, previous_epoch)?;
    let current_indices = participation_cache
        .get_unslashed_participating_indices(TIMELY_TARGET_FLAG_INDEX, current_epoch)?;
    let total_active_balance = participation_cache.current_epoch_total_active_balance();
    let previous_target_balance = previous_indices.total_balance()?;
    let current_target_balance = current_indices.total_balance()?;
    weigh_justification_and_finalization(
        justification_and_finalization_state,
        total_active_balance,
        previous_target_balance,
        current_target_balance,
    )
}
