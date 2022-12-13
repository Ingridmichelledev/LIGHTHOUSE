use crate::test_utils::TestRandom;
use crate::*;
use serde_derive::{Deserialize, Serialize};
use ssz_derive::{Decode, Encode};
use test_random_derive::TestRandom;
use tree_hash_derive::TreeHash;

#[cfg_attr(feature = "arbitrary-fuzz", derive(arbitrary::Arbitrary))]
#[derive(
    Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, Encode, Decode, TreeHash, TestRandom,
)]
pub struct Withdrawal {
    #[serde(with = "eth2_serde_utils::quoted_u64")]
    pub index: u64,
    #[serde(with = "eth2_serde_utils::quoted_u64")]
    pub validator_index: u64,
    pub address: Address,
    #[serde(with = "eth2_serde_utils::quoted_u64")]
    pub amount: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    ssz_and_tree_hash_tests!(Withdrawal);
}
