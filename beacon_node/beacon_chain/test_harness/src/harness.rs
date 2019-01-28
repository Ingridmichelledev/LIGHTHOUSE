use super::TestValidator;
pub use beacon_chain::dump::{Error as DumpError, SlotDump};
use beacon_chain::BeaconChain;
use db::{
    stores::{BeaconBlockStore, BeaconStateStore},
    MemoryDB,
};
use slot_clock::TestingSlotClock;
use std::fs::File;
use std::io::prelude::*;
use std::sync::Arc;
use types::{BeaconBlock, ChainSpec, FreeAttestation, Keypair, Validator};

pub struct BeaconChainHarness {
    pub db: Arc<MemoryDB>,
    pub beacon_chain: Arc<BeaconChain<MemoryDB, TestingSlotClock>>,
    pub block_store: Arc<BeaconBlockStore<MemoryDB>>,
    pub state_store: Arc<BeaconStateStore<MemoryDB>>,
    pub validators: Vec<TestValidator>,
    pub spec: ChainSpec,
}

impl BeaconChainHarness {
    pub fn new(mut spec: ChainSpec, validator_count: usize) -> Self {
        let db = Arc::new(MemoryDB::open());
        let block_store = Arc::new(BeaconBlockStore::new(db.clone()));
        let state_store = Arc::new(BeaconStateStore::new(db.clone()));

        let slot_clock = TestingSlotClock::new(0);

        // Remove the validators present in the spec (if any).
        spec.initial_validators = Vec::with_capacity(validator_count);
        spec.initial_balances = Vec::with_capacity(validator_count);

        // Insert `validator_count` new `Validator` records into the spec, retaining the keypairs
        // for later user.
        let mut keypairs = Vec::with_capacity(validator_count);
        for _ in 0..validator_count {
            let keypair = Keypair::random();

            spec.initial_validators.push(Validator {
                pubkey: keypair.pk.clone(),
                ..std::default::Default::default()
            });
            spec.initial_balances.push(32_000_000_000); // 32 ETH

            keypairs.push(keypair);
        }

        // Create the Beacon Chain
        let beacon_chain = Arc::new(
            BeaconChain::genesis(
                state_store.clone(),
                block_store.clone(),
                slot_clock,
                spec.clone(),
            )
            .unwrap(),
        );

        // Spawn the test validator instances.
        let mut validators = Vec::with_capacity(validator_count);
        for keypair in keypairs {
            validators.push(TestValidator::new(keypair.clone(), beacon_chain.clone()));
        }

        Self {
            db,
            beacon_chain,
            block_store,
            state_store,
            validators,
            spec,
        }
    }

    /// Move the `slot_clock` for the `BeaconChain` forward one slot.
    ///
    /// This is the equivalent of advancing a system clock forward one `SLOT_DURATION`.
    pub fn increment_beacon_chain_slot(&mut self) {
        let slot = self
            .beacon_chain
            .present_slot()
            .expect("Unable to determine slot.")
            + 1;
        self.beacon_chain.slot_clock.set_slot(slot);
    }

    /// Gather the `FreeAttestation`s from the valiators.
    ///
    /// Note: validators will only produce attestations _once per slot_. So, if you call this twice
    /// you'll only get attestations on the first run.
    pub fn gather_free_attesations(&mut self) -> Vec<FreeAttestation> {
        let present_slot = self.beacon_chain.present_slot().unwrap();

        let mut free_attestations = vec![];
        for validator in &mut self.validators {
            // Advance the validator slot.
            validator.set_slot(present_slot);

            // Prompt the validator to produce an attestation (if required).
            if let Ok(free_attestation) = validator.produce_free_attestation() {
                free_attestations.push(free_attestation);
            }
        }
        free_attestations
    }

    /// Get the block from the proposer for the slot.
    ///
    /// Note: the validator will only produce it _once per slot_. So, if you call this twice you'll
    /// only get a block once.
    pub fn produce_block(&mut self) -> BeaconBlock {
        let present_slot = self.beacon_chain.present_slot().unwrap();

        let proposer = self
            .beacon_chain
            .block_proposer(present_slot)
            .expect("Unable to determine proposer.");

        self.validators[proposer].produce_block().unwrap()
    }

    /// Advances the chain with a BeaconBlock and attestations from all validators.
    ///
    /// This is the ideal scenario for the Beacon Chain, 100% honest participation from
    /// validators.
    pub fn advance_chain_with_block(&mut self) {
        self.increment_beacon_chain_slot();
        let free_attestations = self.gather_free_attesations();
        for free_attestation in free_attestations {
            self.beacon_chain
                .process_free_attestation(free_attestation.clone())
                .unwrap();
        }
        let block = self.produce_block();
        self.beacon_chain.process_block(block).unwrap();
    }

    pub fn chain_dump(&self) -> Result<Vec<SlotDump>, DumpError> {
        self.beacon_chain.chain_dump()
    }

    pub fn dump_to_file(&self, filename: String, chain_dump: &Vec<SlotDump>) {
        let json = serde_json::to_string(chain_dump).unwrap();
        let mut file = File::create(filename).unwrap();
        file.write_all(json.as_bytes())
            .expect("Failed writing dump to file.");
    }
}
