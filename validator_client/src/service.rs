/// The Validator Client service.
///
/// Connects to a beacon node and negotiates the correct chain id.
///
/// Once connected, the service loads known validators keypairs from disk. Every slot,
/// the service pings the beacon node, asking for new duties for each of the validators.
///
/// When a validator needs to either produce a block or sign an attestation, it requests the
/// data from the beacon node and performs the signing before publishing the block to the beacon
/// node.
use crate::attester_service::{AttestationGrpcClient, AttesterService};
use crate::block_producer::{BeaconBlockGrpcClient, BlockProducer};
use crate::config::Config as ValidatorConfig;
use crate::duties::{BeaconNodeDuties, DutiesManager, EpochDutiesMap, UpdateOutcome};
use crate::error as error_chain;
use crate::error::ErrorKind;
use crate::signer::Signer;
use attester::test_utils::EpochMap;
use attester::{test_utils::LocalSigner as AttesterLocalSigner, Attester};
use bls::Keypair;
use grpcio::{ChannelBuilder, EnvBuilder};
use protos::services::Empty;
use protos::services_grpc::{
    AttestationServiceClient, BeaconBlockServiceClient, BeaconNodeServiceClient,
    ValidatorServiceClient,
};
use slog::{debug, error, info, warn};
use slot_clock::{SlotClock, SystemTimeSlotClock};
use std::sync::Arc;
use std::sync::RwLock;
use std::thread;
use std::time::{Duration, Instant, SystemTime};
use tokio::prelude::*;
use tokio::runtime::Builder;
use tokio::timer::Interval;
use tokio_timer::clock::Clock;
use types::test_utils::generate_deterministic_keypairs;
use types::{ChainSpec, Epoch, Fork, Slot};

/// The validator service. This is the main thread that executes and maintains validator
/// duties.
//TODO: Generalize the BeaconNode types to use testing
pub struct Service<B: BeaconNodeDuties + 'static, S: Signer + 'static> {
    /// The node's current fork version we are processing on.
    fork: Fork,
    /// The slot clock for this service.
    slot_clock: SystemTimeSlotClock,
    /// The current slot we are processing.
    current_slot: Slot,
    /// The chain specification for this clients instance.
    spec: Arc<ChainSpec>,
    /// The duties manager which maintains the state of when to perform actions.
    duties_manager: Arc<DutiesManager<B, S>>,
    // GRPC Clients
    /// The beacon block GRPC client.
    beacon_block_client: Arc<BeaconBlockGrpcClient>,
    /// The attester GRPC client.
    attester_client: Arc<AttestationServiceClient>,
    /// The validator client logger.
    log: slog::Logger,
}

impl<B: BeaconNodeDuties + 'static, S: Signer + 'static> Service<B, S> {
    ///  Initial connection to the beacon node to determine its properties.
    ///
    ///  This tries to connect to a beacon node. Once connected, it initialised the gRPC clients
    ///  and returns an instance of the service.
    fn initialize_service(
        config: ValidatorConfig,
        log: slog::Logger,
    ) -> error_chain::Result<Service<ValidatorServiceClient, Keypair>> {
        // initialise the beacon node client to check for a connection

        let env = Arc::new(EnvBuilder::new().build());
        // Beacon node gRPC beacon node endpoints.
        let beacon_node_client = {
            let ch = ChannelBuilder::new(env.clone()).connect(&config.server);
            BeaconNodeServiceClient::new(ch)
        };

        // retrieve node information and validate the beacon node
        let node_info = loop {
            match beacon_node_client.info(&Empty::new()) {
                Err(e) => {
                    warn!(log, "Could not connect to node. Error: {}", e);
                    info!(log, "Retrying in 5 seconds...");
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                Ok(info) => {
                    // verify the node's genesis time
                    if SystemTime::now()
                        .duration_since(SystemTime::UNIX_EPOCH)
                        .unwrap()
                        .as_secs()
                        < info.genesis_time
                    {
                        error!(
                            log,
                            "Beacon Node's genesis time is in the future. No work to do.\n Exiting"
                        );
                        return Err("Genesis time in the future".into());
                    }
                    // verify the node's chain id
                    if config.spec.chain_id != info.chain_id as u8 {
                        error!(
                            log,
                            "Beacon Node's genesis time is in the future. No work to do.\n Exiting"
                        );
                        return Err(format!("Beacon node has the wrong chain id. Expected chain id: {}, node's chain id: {}", config.spec.chain_id, info.chain_id).into());
                    }
                    break info;
                }
            };
        };

        // build requisite objects to form Self
        let genesis_time = node_info.get_genesis_time();
        let genesis_slot = Slot::from(node_info.get_genesis_slot());

        info!(log,"Beacon node connected"; "Node Version" => node_info.version.clone(), "Chain ID" => node_info.chain_id, "Genesis time" => genesis_time);

        let proto_fork = node_info.get_fork();
        let mut previous_version: [u8; 4] = [0; 4];
        let mut current_version: [u8; 4] = [0; 4];
        previous_version.copy_from_slice(&proto_fork.get_previous_version()[..4]);
        current_version.copy_from_slice(&proto_fork.get_current_version()[..4]);
        let fork = Fork {
            previous_version,
            current_version,
            epoch: Epoch::from(proto_fork.get_epoch()),
        };

        // initialize the RPC clients

        // Beacon node gRPC beacon block endpoints.
        let beacon_block_client = {
            let ch = ChannelBuilder::new(env.clone()).connect(&config.server);
            let beacon_block_service_client = Arc::new(BeaconBlockServiceClient::new(ch));
            // a wrapper around the service client to implement the beacon block node trait
            Arc::new(BeaconBlockGrpcClient::new(beacon_block_service_client))
        };

        // Beacon node gRPC validator endpoints.
        let validator_client = {
            let ch = ChannelBuilder::new(env.clone()).connect(&config.server);
            Arc::new(ValidatorServiceClient::new(ch))
        };

        //Beacon node gRPC attester endpoints.
        let attester_client = {
            let ch = ChannelBuilder::new(env.clone()).connect(&config.server);
            Arc::new(AttestationServiceClient::new(ch))
        };

        // build the validator slot clock
        let slot_clock =
            SystemTimeSlotClock::new(genesis_slot, genesis_time, config.spec.seconds_per_slot)
                .expect("Unable to instantiate SystemTimeSlotClock.");

        let current_slot = slot_clock
            .present_slot()
            .map_err(|e| ErrorKind::SlotClockError(e))?
            .expect("Genesis must be in the future");

        /* Generate the duties manager */

        // generate keypairs

        // TODO: keypairs are randomly generated; they should be loaded from a file or generated.
        // https://github.com/sigp/lighthouse/issues/160
        let keypairs = Arc::new(generate_deterministic_keypairs(8));

        // Builds a mapping of Epoch -> Map(PublicKey, EpochDuty)
        // where EpochDuty contains slot numbers and attestation data that each validator needs to
        // produce work on.
        let duties_map = RwLock::new(EpochDutiesMap::new(config.spec.slots_per_epoch));

        // builds a manager which maintains the list of current duties for all known validators
        // and can check when a validator needs to perform a task.
        let duties_manager = Arc::new(DutiesManager {
            duties_map,
            // these are abstract objects capable of signing
            signers: keypairs,
            beacon_node: validator_client,
        });

        let spec = Arc::new(config.spec);

        Ok(Service {
            fork,
            slot_clock,
            current_slot,
            spec,
            duties_manager,
            beacon_block_client,
            attester_client,
            log,
        })
    }

    /// Initialise the service then run the core thread.
    // TODO: Improve handling of generic BeaconNode types, to stub grpcClient
    pub fn start(config: ValidatorConfig, log: slog::Logger) -> error_chain::Result<()> {
        // connect to the node and retrieve its properties and initialize the gRPC clients
        let mut service =
            Service::<ValidatorServiceClient, Keypair>::initialize_service(config, log)?;

        // we have connected to a node and established its parameters. Spin up the core service

        // set up the validator service runtime
        let mut runtime = Builder::new()
            .clock(Clock::system())
            .name_prefix("validator-client-")
            .build()
            .map_err(|e| format!("Tokio runtime failed: {}", e))?;

        let duration_to_next_slot = service
            .slot_clock
            .duration_to_next_slot()
            .map_err(|e| format!("System clock error: {:?}", e))?
            .expect("Cannot start before genesis");

        // set up the validator work interval - start at next slot and proceed every slot
        let interval = {
            // Set the interval to start at the next slot, and every slot after
            let slot_duration = Duration::from_secs(service.spec.seconds_per_slot);
            //TODO: Handle checked add correctly
            Interval::new(Instant::now() + duration_to_next_slot, slot_duration)
        };

        /* kick off the core service */
        runtime.block_on(
            interval
                .for_each(move |_| {
                    // if a non-fatal error occurs, proceed to the next slot.
                    let _ignore_error = service.per_slot_execution();
                    // completed a slot process
                    Ok(())
                })
                .map_err(|e| format!("Service thread failed: {:?}", e)),
        )?;
        // validator client exited
        Ok(())
    }

    /// The execution logic that runs every slot.
    // Errors are logged to output, and core execution continues unless fatal errors occur.
    fn per_slot_execution(&mut self) -> error_chain::Result<()> {
        /* get the new current slot and epoch */
        self.update_current_slot()?;

        /* check for new duties */
        self.check_for_duties();

        /* process any required duties for validators */
        self.process_duties();

        Ok(())
    }

    /// Updates the known current slot and epoch.
    fn update_current_slot(&mut self) -> error_chain::Result<()> {
        let current_slot = match self.slot_clock.present_slot() {
            Err(e) => {
                error!(self.log, "SystemTimeError {:?}", e);
                return Err("Could not read system time".into());
            }
            Ok(slot) => slot.expect("Genesis is in the future"),
        };

        let current_epoch = current_slot.epoch(self.spec.slots_per_epoch);

        // this is a fatal error. If the slot clock repeats, there is something wrong with
        // the timer, terminate immediately.
        assert!(
            current_slot > self.current_slot,
            "The Timer should poll a new slot"
        );
        self.current_slot = current_slot;
        info!(self.log, "Processing"; "slot" => current_slot.as_u64(), "epoch" => current_epoch.as_u64());
        Ok(())
    }

    /// For all known validator keypairs, update any known duties from the beacon node.
    fn check_for_duties(&mut self) {
        let cloned_manager = self.duties_manager.clone();
        let cloned_log = self.log.clone();
        let current_epoch = self.current_slot.epoch(self.spec.slots_per_epoch);
        // spawn a new thread separate to the runtime
        // TODO: Handle thread termination/timeout
        std::thread::spawn(move || {
            // the return value is a future which returns ready.
            // built to be compatible with the tokio runtime.
            let _empty = cloned_manager.run_update(current_epoch.clone(), cloned_log.clone());
        });
    }

    /// If there are any duties to process, spawn a separate thread and perform required actions.
    fn process_duties(&mut self) {
        if let Some(work) = self.duties_manager.get_current_work(self.current_slot) {
            for (signer_index, work_type) in work {
                if work_type.produce_block {
                    // spawns a thread to produce a beacon block
                    let signers = self.duties_manager.signers.clone();
                    let fork = self.fork.clone();
                    let slot = self.current_slot.clone();
                    let spec = self.spec.clone();
                    let beacon_node = self.beacon_block_client.clone();
                    std::thread::spawn(move || {
                        let signer = &signers[signer_index];
                        let block_producer = BlockProducer {
                            fork,
                            slot,
                            spec,
                            beacon_node,
                            signer,
                        };
                    });

                    // TODO: Produce a beacon block in a new thread
                }
                if work_type.attestation_duty.is_some() {
                    // available AttestationDuty info
                    /*
                    let attestation_duty =
                        work_type.attestation_duty.expect("Cannot be None");
                    let attester_grpc_client = Arc::new(AttestationGrpcClient::new(
                        service.attester_client.clone(),
                    ));
                    let signer = Arc::new(AttesterLocalSigner::new(keypair.clone()));
                    let attester = Attester::new(attester_grpc_client, signer);
                    let mut attester_service = AttesterService {
                        attester,
                        poll_interval_millis: POLL_INTERVAL_MILLIS,
                        log: log.clone(),
                    };
                    attester_service.run();
                    */
                }
            }
        }
    }
}
