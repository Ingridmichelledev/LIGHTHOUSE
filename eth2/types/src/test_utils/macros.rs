#[cfg(test)]
#[macro_export]
macro_rules! ssz_tests {
    ($type: ident) => {
        #[test]
        pub fn test_ssz_round_trip() {
            use crate::test_utils::{SeedableRng, TestRandom, XorShiftRng};
            use ssz::{ssz_encode, Decodable};

            let mut rng = XorShiftRng::from_seed([42; 16]);
            let original = $type::random_for_test(&mut rng);

            let bytes = ssz_encode(&original);
            let (decoded, _): ($type, usize) = <_>::ssz_decode(&bytes, 0).unwrap();

            assert_eq!(original, decoded);
        }

        #[test]
        pub fn test_tree_hash_root() {
            use crate::test_utils::{SeedableRng, TestRandom, XorShiftRng};
            use tree_hash::TreeHash;

            let mut rng = XorShiftRng::from_seed([42; 16]);
            let original = $type::random_for_test(&mut rng);

            let result = original.tree_hash_root();

            assert_eq!(result.len(), 32);
            // TODO: Add further tests
            // https://github.com/sigp/lighthouse/issues/170
        }
    };
}
