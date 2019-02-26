#![cfg(test)]
use crate::EpochProcessable;
use env_logger::{Builder, Env};
use types::beacon_state::BeaconStateBuilder;
use types::*;

#[test]
fn runs_without_error() {
    Builder::from_env(Env::default().default_filter_or("error")).init();

    let mut builder = BeaconStateBuilder::with_random_validators(8);
    builder.spec = ChainSpec::few_validators();

    builder.genesis().unwrap();
    builder.teleport_to_end_of_epoch(builder.spec.genesis_epoch + 4);

    let mut state = builder.build().unwrap();

    let spec = &builder.spec;
    state.per_epoch_processing(spec).unwrap();
}
