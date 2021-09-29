//! This crate provides an abstraction over one or more *execution engines*. An execution engine
//! was formerly known as an "eth1 node", like Geth, Nethermind, Erigon, etc.
//!
//! This crate only provides useful functionality for "The Merge", it does not provide any of the
//! deposit-contract functionality that the `beacon_node/eth1` crate already provides.

use engine_api::{Error as ApiError, *};
use engines::{Engine, EngineError, Engines};
use lru::LruCache;
use sensitive_url::SensitiveUrl;
use slog::{crit, Logger};
use std::future::Future;
use std::sync::Arc;
use task_executor::TaskExecutor;
use tokio::sync::{Mutex, MutexGuard};

pub use engine_api::{http::HttpJsonRpc, ConsensusStatus, ExecutePayloadResponse};
pub use execute_payload_handle::ExecutePayloadHandle;

mod engine_api;
mod engines;
mod execute_payload_handle;
pub mod test_utils;

/// Each time the `ExecutionLayer` retrieves a block from an execution node, it stores that block
/// in an LRU cache to avoid redundant lookups. This is the size of that cache.
const EXECUTION_BLOCKS_LRU_CACHE_SIZE: usize = 128;

#[derive(Debug)]
pub enum Error {
    NoEngines,
    ApiError(ApiError),
    EngineErrors(Vec<EngineError>),
    NotSynced,
    ShuttingDown,
    FeeRecipientUnspecified,
}

impl From<ApiError> for Error {
    fn from(e: ApiError) -> Self {
        Error::ApiError(e)
    }
}

struct Inner {
    engines: Engines<HttpJsonRpc>,
    terminal_total_difficulty: Uint256,
    terminal_block_hash: Hash256,
    fee_recipient: Option<Address>,
    execution_blocks: Mutex<LruCache<Hash256, ExecutionBlock>>,
    executor: TaskExecutor,
    log: Logger,
}

/// Provides access to one or more execution engines and provides a neat interface for consumption
/// by the `BeaconChain`.
///
/// When there is more than one execution node specified, the others will be used in a "fallback"
/// fashion. Some requests may be broadcast to all nodes and others might only be sent to the first
/// node that returns a valid response. Ultimately, the purpose of fallback nodes is to provide
/// redundancy in the case where one node is offline.
///
/// The fallback nodes have an ordering. The first supplied will be the first contacted, and so on.
#[derive(Clone)]
pub struct ExecutionLayer {
    inner: Arc<Inner>,
}

impl ExecutionLayer {
    /// Instantiate `Self` with `urls.len()` engines, all using the JSON-RPC via HTTP.
    pub fn from_urls(
        urls: Vec<SensitiveUrl>,
        terminal_total_difficulty: Uint256,
        terminal_block_hash: Hash256,
        fee_recipient: Option<Address>,
        executor: TaskExecutor,
        log: Logger,
    ) -> Result<Self, Error> {
        if urls.is_empty() {
            return Err(Error::NoEngines);
        }

        let engines = urls
            .into_iter()
            .map(|url| {
                let id = url.to_string();
                let api = HttpJsonRpc::new(url)?;
                Ok(Engine::new(id, api))
            })
            .collect::<Result<_, ApiError>>()?;

        let inner = Inner {
            engines: Engines {
                engines,
                log: log.clone(),
            },
            terminal_total_difficulty,
            terminal_block_hash,
            fee_recipient,
            execution_blocks: Mutex::new(LruCache::new(EXECUTION_BLOCKS_LRU_CACHE_SIZE)),
            executor,
            log,
        };

        Ok(Self {
            inner: Arc::new(inner),
        })
    }
}

impl ExecutionLayer {
    fn engines(&self) -> &Engines<HttpJsonRpc> {
        &self.inner.engines
    }

    fn executor(&self) -> &TaskExecutor {
        &self.inner.executor
    }

    fn terminal_total_difficulty(&self) -> Uint256 {
        self.inner.terminal_total_difficulty
    }

    fn terminal_block_hash(&self) -> Hash256 {
        self.inner.terminal_block_hash
    }

    fn fee_recipient(&self) -> Result<Address, Error> {
        self.inner
            .fee_recipient
            .ok_or(Error::FeeRecipientUnspecified)
    }

    /// Note: this function returns a mutex guard, be careful to avoid deadlocks.
    async fn execution_blocks(&self) -> MutexGuard<'_, LruCache<Hash256, ExecutionBlock>> {
        self.inner.execution_blocks.lock().await
    }

    fn log(&self) -> &Logger {
        &self.inner.log
    }

    /// Convenience function to allow calling async functions in a non-async context.
    pub fn block_on<'a, T, U, V>(&'a self, generate_future: T) -> Result<V, Error>
    where
        T: Fn(&'a Self) -> U,
        U: Future<Output = Result<V, Error>>,
    {
        let runtime = self
            .executor()
            .runtime()
            .upgrade()
            .ok_or(Error::ShuttingDown)?;
        // TODO(paul): respect the shutdown signal.
        runtime.block_on(generate_future(self))
    }

    /// Convenience function to allow spawning a task without waiting for the result.
    pub fn spawn<T, U>(&self, generate_future: T, name: &'static str)
    where
        T: FnOnce(Self) -> U,
        U: Future<Output = ()> + Send + 'static,
    {
        self.executor().spawn(generate_future(self.clone()), name);
    }

    /// Maps to the `engine_preparePayload` JSON-RPC function.
    ///
    /// ## Fallback Behavior
    ///
    /// The result will be returned from the first node that returns successfully. No more nodes
    /// will be contacted.
    pub async fn prepare_payload(
        &self,
        parent_hash: Hash256,
        timestamp: u64,
        random: Hash256,
    ) -> Result<PayloadId, Error> {
        let fee_recipient = self.fee_recipient()?;
        self.engines()
            .first_success(|engine| {
                // TODO(merge): make a cache for these IDs, so we don't always have to perform this
                // request.
                engine
                    .api
                    .prepare_payload(parent_hash, timestamp, random, fee_recipient)
            })
            .await
            .map_err(Error::EngineErrors)
    }

    /// Maps to the `engine_getPayload` JSON-RPC call.
    ///
    /// However, it will attempt to call `self.prepare_payload` if it cannot find an existing
    /// payload id for the given parameters.
    ///
    /// ## Fallback Behavior
    ///
    /// The result will be returned from the first node that returns successfully. No more nodes
    /// will be contacted.
    pub async fn get_payload<T: EthSpec>(
        &self,
        parent_hash: Hash256,
        timestamp: u64,
        random: Hash256,
    ) -> Result<ExecutionPayload<T>, Error> {
        let fee_recipient = self.fee_recipient()?;
        self.engines()
            .first_success(|engine| async move {
                // TODO(merge): make a cache for these IDs, so we don't always have to perform this
                // request.
                let payload_id = engine
                    .api
                    .prepare_payload(parent_hash, timestamp, random, fee_recipient)
                    .await?;

                engine.api.get_payload(payload_id).await
            })
            .await
            .map_err(Error::EngineErrors)
    }

    /// Maps to the `engine_executePayload` JSON-RPC call.
    ///
    /// ## Fallback Behaviour
    ///
    /// The request will be broadcast to all nodes, simultaneously. It will await a response (or
    /// failure) from all nodes and then return based on the first of these conditions which
    /// returns true:
    ///
    /// - Valid, if any nodes return valid.
    /// - Invalid, if any nodes return invalid.
    /// - Syncing, if any nodes return syncing.
    /// - An error, if all nodes return an error.
    pub async fn execute_payload<T: EthSpec>(
        &self,
        execution_payload: &ExecutionPayload<T>,
    ) -> Result<(ExecutePayloadResponse, ExecutePayloadHandle), Error> {
        let broadcast_results = self
            .engines()
            .broadcast(|engine| engine.api.execute_payload(execution_payload.clone()))
            .await;

        let mut errors = vec![];
        let mut valid = 0;
        let mut invalid = 0;
        let mut syncing = 0;
        for result in broadcast_results {
            match result {
                Ok(ExecutePayloadResponse::Valid) => valid += 1,
                Ok(ExecutePayloadResponse::Invalid) => invalid += 1,
                Ok(ExecutePayloadResponse::Syncing) => syncing += 1,
                Err(e) => errors.push(e),
            }
        }

        if valid > 0 && invalid > 0 {
            crit!(
                self.log(),
                "Consensus failure between execution nodes";
                "method" => "execute_payload"
            );
        }

        let execute_payload_response = if valid > 0 {
            ExecutePayloadResponse::Valid
        } else if invalid > 0 {
            ExecutePayloadResponse::Invalid
        } else if syncing > 0 {
            ExecutePayloadResponse::Syncing
        } else {
            return Err(Error::EngineErrors(errors));
        };

        let execute_payload_handle = ExecutePayloadHandle {
            block_hash: execution_payload.block_hash,
            execution_layer: Some(self.clone()),
            log: self.log().clone(),
        };

        Ok((execute_payload_response, execute_payload_handle))
    }

    /// Maps to the `engine_consensusValidated` JSON-RPC call.
    ///
    /// ## Fallback Behaviour
    ///
    /// The request will be broadcast to all nodes, simultaneously. It will await a response (or
    /// failure) from all nodes and then return based on the first of these conditions which
    /// returns true:
    ///
    /// - Ok, if any node returns successfully.
    /// - An error, if all nodes return an error.
    pub async fn consensus_validated(
        &self,
        block_hash: Hash256,
        status: ConsensusStatus,
    ) -> Result<(), Error> {
        let broadcast_results = self
            .engines()
            .broadcast(|engine| engine.api.consensus_validated(block_hash, status))
            .await;

        if broadcast_results.iter().any(Result::is_ok) {
            Ok(())
        } else {
            Err(Error::EngineErrors(
                broadcast_results
                    .into_iter()
                    .filter_map(Result::err)
                    .collect(),
            ))
        }
    }

    /// Maps to the `engine_consensusValidated` JSON-RPC call.
    ///
    /// ## Fallback Behaviour
    ///
    /// The request will be broadcast to all nodes, simultaneously. It will await a response (or
    /// failure) from all nodes and then return based on the first of these conditions which
    /// returns true:
    ///
    /// - Ok, if any node returns successfully.
    /// - An error, if all nodes return an error.
    pub async fn forkchoice_updated(
        &self,
        head_block_hash: Hash256,
        finalized_block_hash: Hash256,
    ) -> Result<(), Error> {
        let broadcast_results = self
            .engines()
            .broadcast(|engine| {
                engine
                    .api
                    .forkchoice_updated(head_block_hash, finalized_block_hash)
            })
            .await;

        if broadcast_results.iter().any(Result::is_ok) {
            Ok(())
        } else {
            Err(Error::EngineErrors(
                broadcast_results
                    .into_iter()
                    .filter_map(Result::err)
                    .collect(),
            ))
        }
    }

    /// Used during block production to determine if the merge has been triggered.
    ///
    /// ## Specification
    ///
    /// `get_terminal_pow_block_hash`
    ///
    /// https://github.com/ethereum/consensus-specs/blob/v1.1.0/specs/merge/validator.md
    pub async fn get_terminal_pow_block_hash(&self) -> Result<Option<Hash256>, Error> {
        self.engines()
            .first_success(|engine| async move {
                if self.terminal_block_hash() != Hash256::zero() {
                    // Note: the specification is written such that if there are multiple blocks in
                    // the PoW chain with the terminal block hash, then to select 0'th one.
                    //
                    // Whilst it's not clear what the 0'th block is, we ignore this completely and
                    // make the assumption that there are no two blocks in the chain with the same
                    // hash. Such a scenario would be a devestating hash collision with external
                    // implications far outweighing those here.
                    Ok(self
                        .get_pow_block(engine, self.terminal_block_hash())
                        .await?
                        .map(|block| block.block_hash))
                } else {
                    self.get_pow_block_hash_at_total_difficulty(engine).await
                }
            })
            .await
            .map_err(Error::EngineErrors)
    }

    /// This function should remain internal. External users should use
    /// `self.get_terminal_pow_block` instead, since it checks against the terminal block hash
    /// override.
    ///
    /// ## Specification
    ///
    /// `get_pow_block_at_terminal_total_difficulty`
    ///
    /// https://github.com/ethereum/consensus-specs/blob/v1.1.0/specs/merge/validator.md
    async fn get_pow_block_hash_at_total_difficulty(
        &self,
        engine: &Engine<HttpJsonRpc>,
    ) -> Result<Option<Hash256>, ApiError> {
        let mut ttd_exceeding_block = None;
        let mut block = engine
            .api
            .get_block_by_number(BlockByNumberQuery::Tag(LATEST_TAG))
            .await?
            .ok_or(ApiError::ExecutionHeadBlockNotFound)?;

        self.execution_blocks().await.put(block.block_hash, block);

        // TODO(merge): This function can theoretically loop indefinitely, as per the
        // specification. We should consider how to fix this. See discussion:
        //
        // https://github.com/ethereum/consensus-specs/issues/2636
        loop {
            if block.total_difficulty >= self.terminal_total_difficulty() {
                ttd_exceeding_block = Some(block.block_hash);

                // Try to prevent infinite loops.
                if block.block_hash == block.parent_hash {
                    return Err(ApiError::ParentHashEqualsBlockHash(block.block_hash));
                }

                block = self
                    .get_pow_block(engine, block.parent_hash)
                    .await?
                    .ok_or(ApiError::ExecutionBlockNotFound(block.parent_hash))?;
            } else {
                return Ok(ttd_exceeding_block);
            }
        }
    }

    /// Used during block verification to check that a block correctly triggers the merge.
    ///
    /// ## Returns
    ///
    /// - `Some(true)` if the given `block_hash` is the terminal proof-of-work block.
    /// - `Some(false)` if the given `block_hash` is certainly *not* the terminal proof-of-work
    ///     block.
    /// - `None` if the `block_hash` or its parent were not present on the execution engines.
    /// - `Err(_)` if there was an error connecting to the execution engines.
    ///
    /// ## Fallback Behaviour
    ///
    /// The request will be broadcast to all nodes, simultaneously. It will await a response (or
    /// failure) from all nodes and then return based on the first of these conditions which
    /// returns true:
    ///
    /// - Terminal, if any node indicates it is terminal.
    /// - Not terminal, if any node indicates it is non-terminal.
    /// - Block not found, if any node cannot find the block.
    /// - An error, if all nodes return an error.
    ///
    /// ## Specification
    ///
    /// `is_valid_terminal_pow_block`
    ///
    /// https://github.com/ethereum/consensus-specs/blob/v1.1.0/specs/merge/fork-choice.md
    pub async fn is_valid_terminal_pow_block_hash(
        &self,
        block_hash: Hash256,
    ) -> Result<Option<bool>, Error> {
        let broadcast_results = self
            .engines()
            .broadcast(|engine| async move {
                if let Some(pow_block) = self.get_pow_block(engine, block_hash).await? {
                    if let Some(pow_parent) =
                        self.get_pow_block(engine, pow_block.parent_hash).await?
                    {
                        return Ok(Some(
                            self.is_valid_terminal_pow_block(pow_block, pow_parent),
                        ));
                    }
                }

                Ok(None)
            })
            .await;

        let mut errors = vec![];
        let mut terminal = 0;
        let mut not_terminal = 0;
        let mut block_missing = 0;
        for result in broadcast_results {
            match result {
                Ok(Some(true)) => terminal += 1,
                Ok(Some(false)) => not_terminal += 1,
                Ok(None) => block_missing += 1,
                Err(e) => errors.push(e),
            }
        }

        if terminal > 0 && not_terminal > 0 {
            crit!(
                self.log(),
                "Consensus failure between execution nodes";
                "method" => "is_valid_terminal_pow_block_hash"
            );
        }

        if terminal > 0 {
            Ok(Some(true))
        } else if not_terminal > 0 {
            Ok(Some(false))
        } else if block_missing > 0 {
            Ok(None)
        } else {
            Err(Error::EngineErrors(errors))
        }
    }

    /// This function should remain internal.
    ///
    /// External users should use `self.is_valid_terminal_pow_block_hash`.
    fn is_valid_terminal_pow_block(&self, block: ExecutionBlock, parent: ExecutionBlock) -> bool {
        if block.block_hash == self.terminal_block_hash() {
            return true;
        }

        let is_total_difficulty_reached =
            block.total_difficulty >= self.terminal_total_difficulty();
        let is_parent_total_difficulty_valid =
            parent.total_difficulty < self.terminal_total_difficulty();
        is_total_difficulty_reached && is_parent_total_difficulty_valid
    }

    /// Maps to the `eth_getBlockByHash` JSON-RPC call.
    ///
    /// ## TODO(merge)
    ///
    /// This will return an execution block regardless of whether or not it was created by a PoW
    /// miner (pre-merge) or a PoS validator (post-merge). It's not immediately clear if this is
    /// correct or not, see the discussion here:
    ///
    /// https://github.com/ethereum/consensus-specs/issues/2636
    async fn get_pow_block(
        &self,
        engine: &Engine<HttpJsonRpc>,
        hash: Hash256,
    ) -> Result<Option<ExecutionBlock>, ApiError> {
        if let Some(cached) = self.execution_blocks().await.get(&hash).copied() {
            // The block was in the cache, no need to request it from the execution
            // engine.
            return Ok(Some(cached));
        }

        // The block was *not* in the cache, request it from the execution
        // engine and cache it for future reference.
        if let Some(block) = engine.api.get_block_by_hash(hash).await? {
            self.execution_blocks().await.put(hash, block);
            Ok(Some(block))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::test_utils::{MockServer, DEFAULT_TERMINAL_DIFFICULTY};
    use environment::null_logger;
    use types::MainnetEthSpec;

    struct SingleEngineTester {
        server: MockServer<MainnetEthSpec>,
        el: ExecutionLayer,
        runtime: Option<Arc<tokio::runtime::Runtime>>,
        _runtime_shutdown: exit_future::Signal,
    }

    impl SingleEngineTester {
        pub fn new() -> Self {
            let server = MockServer::unit_testing();

            let url = SensitiveUrl::parse(&server.url()).unwrap();
            let log = null_logger().unwrap();

            let runtime = Arc::new(
                tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            );
            let (runtime_shutdown, exit) = exit_future::signal();
            let (shutdown_tx, _) = futures::channel::mpsc::channel(1);
            let executor =
                TaskExecutor::new(Arc::downgrade(&runtime), exit, log.clone(), shutdown_tx);

            let el = ExecutionLayer::from_urls(
                vec![url],
                DEFAULT_TERMINAL_DIFFICULTY.into(),
                Hash256::zero(),
                Some(Address::repeat_byte(42)),
                executor,
                log,
            )
            .unwrap();

            Self {
                server,
                el,
                runtime: Some(runtime),
                _runtime_shutdown: runtime_shutdown,
            }
        }

        pub async fn produce_valid_execution_payload_on_head(self) -> Self {
            let latest_execution_block = {
                let block_gen = self.server.execution_block_generator().await;
                block_gen.latest_block().unwrap()
            };

            let parent_hash = latest_execution_block.block_hash();
            let block_number = latest_execution_block.block_number() + 1;
            let timestamp = block_number;
            let random = Hash256::from_low_u64_be(block_number);

            let _payload_id = self
                .el
                .prepare_payload(parent_hash, timestamp, random)
                .await
                .unwrap();

            let payload = self
                .el
                .get_payload::<MainnetEthSpec>(parent_hash, timestamp, random)
                .await
                .unwrap();
            let block_hash = payload.block_hash;
            assert_eq!(payload.parent_hash, parent_hash);
            assert_eq!(payload.block_number, block_number);
            assert_eq!(payload.timestamp, timestamp);
            assert_eq!(payload.random, random);

            let (payload_response, mut payload_handle) =
                self.el.execute_payload(&payload).await.unwrap();
            assert_eq!(payload_response, ExecutePayloadResponse::Valid);

            payload_handle.publish_async(ConsensusStatus::Valid).await;

            self.el
                .forkchoice_updated(block_hash, Hash256::zero())
                .await
                .unwrap();

            let head_execution_block = {
                let block_gen = self.server.execution_block_generator().await;
                block_gen.latest_block().unwrap()
            };

            assert_eq!(head_execution_block.block_number(), block_number);
            assert_eq!(head_execution_block.block_hash(), block_hash);
            assert_eq!(head_execution_block.parent_hash(), parent_hash);

            self
        }

        pub async fn move_to_block_prior_to_terminal_block(self) -> Self {
            let target_block = {
                let block_gen = self.server.execution_block_generator().await;
                block_gen.terminal_block_number.checked_sub(1).unwrap()
            };
            self.move_to_pow_block(target_block).await
        }

        pub async fn move_to_terminal_block(self) -> Self {
            let target_block = {
                let block_gen = self.server.execution_block_generator().await;
                block_gen.terminal_block_number
            };
            self.move_to_pow_block(target_block).await
        }

        pub async fn move_to_pow_block(self, target_block: u64) -> Self {
            {
                let mut block_gen = self.server.execution_block_generator().await;
                let next_block = block_gen.latest_block().unwrap().block_number() + 1;
                assert!(target_block >= next_block);

                block_gen
                    .insert_pow_blocks(next_block..=target_block)
                    .unwrap();
            }
            self
        }

        pub async fn with_terminal_block<'a, T, U>(self, func: T) -> Self
        where
            T: Fn(ExecutionLayer, Option<ExecutionBlock>) -> U,
            U: Future<Output = ()>,
        {
            let terminal_block_number = self
                .server
                .execution_block_generator()
                .await
                .terminal_block_number;
            let terminal_block = self
                .server
                .execution_block_generator()
                .await
                .execution_block_by_number(terminal_block_number);

            func(self.el.clone(), terminal_block).await;
            self
        }

        pub fn shutdown(&mut self) {
            if let Some(runtime) = self.runtime.take() {
                Arc::try_unwrap(runtime).unwrap().shutdown_background()
            }
        }
    }

    impl Drop for SingleEngineTester {
        fn drop(&mut self) {
            self.shutdown()
        }
    }

    #[tokio::test]
    async fn produce_three_valid_pos_execution_blocks() {
        SingleEngineTester::new()
            .move_to_terminal_block()
            .await
            .produce_valid_execution_payload_on_head()
            .await
            .produce_valid_execution_payload_on_head()
            .await
            .produce_valid_execution_payload_on_head()
            .await;
    }

    #[tokio::test]
    async fn finds_valid_terminal_block_hash() {
        SingleEngineTester::new()
            .move_to_block_prior_to_terminal_block()
            .await
            .with_terminal_block(|el, _| async move {
                assert_eq!(el.get_terminal_pow_block_hash().await.unwrap(), None)
            })
            .await
            .move_to_terminal_block()
            .await
            .with_terminal_block(|el, terminal_block| async move {
                assert_eq!(
                    el.get_terminal_pow_block_hash().await.unwrap(),
                    Some(terminal_block.unwrap().block_hash)
                )
            })
            .await;
    }

    #[tokio::test]
    async fn verifies_valid_terminal_block_hash() {
        SingleEngineTester::new()
            .move_to_terminal_block()
            .await
            .with_terminal_block(|el, terminal_block| async move {
                assert_eq!(
                    el.is_valid_terminal_pow_block_hash(terminal_block.unwrap().block_hash)
                        .await
                        .unwrap(),
                    Some(true)
                )
            })
            .await;
    }

    #[tokio::test]
    async fn rejects_invalid_terminal_block_hash() {
        SingleEngineTester::new()
            .move_to_terminal_block()
            .await
            .with_terminal_block(|el, terminal_block| async move {
                let invalid_terminal_block = terminal_block.unwrap().parent_hash;

                assert_eq!(
                    el.is_valid_terminal_pow_block_hash(invalid_terminal_block)
                        .await
                        .unwrap(),
                    Some(false)
                )
            })
            .await;
    }

    #[tokio::test]
    async fn rejects_unknown_terminal_block_hash() {
        SingleEngineTester::new()
            .move_to_terminal_block()
            .await
            .with_terminal_block(|el, _| async move {
                let missing_terminal_block = Hash256::repeat_byte(42);

                assert_eq!(
                    el.is_valid_terminal_pow_block_hash(missing_terminal_block)
                        .await
                        .unwrap(),
                    None
                )
            })
            .await;
    }
}
