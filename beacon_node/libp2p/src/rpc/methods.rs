/// Available RPC methods types and ids.
use ssz_derive::{Decode, Encode};
use types::{BeaconBlockBody, BeaconBlockHeader, Epoch, Hash256, Slot};

#[derive(Debug)]
pub enum RPCMethod {
    Hello,
    Goodbye,
    BeaconBlockRoots,
    BeaconBlockHeaders,
    BeaconBlockBodies,
    Unknown,
}

impl From<u16> for RPCMethod {
    fn from(method_id: u16) -> Self {
        match method_id {
            0 => RPCMethod::Hello,
            1 => RPCMethod::Goodbye,
            10 => RPCMethod::BeaconBlockRoots,
            11 => RPCMethod::BeaconBlockHeaders,
            12 => RPCMethod::BeaconBlockBodies,
            _ => RPCMethod::Unknown,
        }
    }
}

impl Into<u16> for RPCMethod {
    fn into(self) -> u16 {
        match self {
            RPCMethod::Hello => 0,
            RPCMethod::Goodbye => 1,
            RPCMethod::BeaconBlockRoots => 10,
            RPCMethod::BeaconBlockHeaders => 11,
            RPCMethod::BeaconBlockBodies => 12,
            _ => 0,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RPCRequest {
    Hello(HelloMessage),
    Goodbye(u64),
    BeaconBlockRoots(BeaconBlockRootsRequest),
    BeaconBlockHeaders(BeaconBlockHeadersRequest),
    BeaconBlockBodies(BeaconBlockBodiesRequest),
}

#[derive(Debug, Clone)]
pub enum RPCResponse {
    Hello(HelloMessage),
    BeaconBlockRoots(BeaconBlockRootsResponse),
    BeaconBlockHeaders(BeaconBlockHeadersResponse),
    BeaconBlockBodies(BeaconBlockBodiesResponse),
}

/* Request/Response data structures for RPC methods */

/// The HELLO request/response handshake message.
#[derive(Encode, Decode, Clone, Debug)]
pub struct HelloMessage {
    /// The network ID of the peer.
    pub network_id: u8,
    /// The peers last finalized root.
    pub latest_finalized_root: Hash256,
    /// The peers last finalized epoch.
    pub latest_finalized_epoch: Epoch,
    /// The peers last block root.
    pub best_root: Hash256,
    /// The peers last slot.
    pub best_slot: Slot,
}

/// Request a number of beacon block roots from a peer.
#[derive(Encode, Decode, Clone, Debug)]
pub struct BeaconBlockRootsRequest {
    /// The starting slot of the requested blocks.
    start_slot: Slot,
    /// The number of blocks from the start slot.
    count: u64, // this must be less than 32768. //TODO: Enforce this in the lower layers
}

/// Response containing a number of beacon block roots from a peer.
#[derive(Encode, Decode, Clone, Debug)]
pub struct BeaconBlockRootsResponse {
    /// List of requested blocks and associated slots.
    roots: Vec<BlockRootSlot>,
}

/// Contains a block root and associated slot.
#[derive(Encode, Decode, Clone, Debug)]
pub struct BlockRootSlot {
    /// The block root.
    block_root: Hash256,
    /// The block slot.
    slot: Slot,
}

/// Request a number of beacon block headers from a peer.
#[derive(Encode, Decode, Clone, Debug)]
pub struct BeaconBlockHeadersRequest {
    /// The starting header hash of the requested headers.
    start_root: Hash256,
    /// The starting slot of the requested headers.
    start_slot: Slot,
    /// The maximum number of headers than can be returned.
    max_headers: u64,
    /// The maximum number of slots to skip between blocks.
    skip_slots: u64,
}

/// Response containing requested block headers.
#[derive(Encode, Decode, Clone, Debug)]
pub struct BeaconBlockHeadersResponse {
    /// The list of requested beacon block headers.
    headers: Vec<BeaconBlockHeader>,
}

/// Request a number of beacon block bodies from a peer.
#[derive(Encode, Decode, Clone, Debug)]
pub struct BeaconBlockBodiesRequest {
    /// The list of beacon block bodies being requested.
    block_roots: Hash256,
}

/// Response containing the list of requested beacon block bodies.
#[derive(Encode, Decode, Clone, Debug)]
pub struct BeaconBlockBodiesResponse {
    /// The list of beacon block bodies being requested.
    block_bodies: Vec<BeaconBlockBody>,
}
