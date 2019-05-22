use super::*;
use crate::case_result::compare_result;
use bls::hash_on_g2;
use serde_derive::Deserialize;
use types::EthSpec;

#[derive(Debug, Clone, Deserialize)]
pub struct BlsG2UncompressedInput {
    pub message: String,
    pub domain: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BlsG2Uncompressed {
    pub input: BlsG2UncompressedInput,
    pub output: Vec<Vec<String>>,
}

impl YamlDecode for BlsG2Uncompressed {
    fn yaml_decode(yaml: &String) -> Result<Self, Error> {
        Ok(serde_yaml::from_str(&yaml.as_str()).unwrap())
    }
}

impl EfTest for Cases<BlsG2Uncompressed> {
    fn test_results<E: EthSpec>(&self) -> Vec<CaseResult> {
        self.test_cases
            .iter()
            .enumerate()
            .map(|(i, tc)| {
                let result = compressed_hash(&tc.input.message, &tc.input.domain, &tc.output);

                CaseResult::new(i, tc, result)
            })
            .collect()
    }
}

/// Execute a `compressed hash to g2` test case.
fn compressed_hash(
    message: &String,
    domain: &String,
    output: &Vec<Vec<String>>,
) -> Result<(), Error> {
    // Convert message and domain to required types
    let msg =
        hex::decode(&message[2..]).map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))?;
    let d = hex::decode(&domain[2..]).map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))?;
    let d = bytes_to_u64(&d);

    // Calculate the point and convert it to compressed bytes
    let point = hash_on_g2(&msg, d);
    let mut point_bytes = [0 as u8; 288];
    point.getpx().geta().tobytearray(&mut point_bytes, 0);
    point.getpx().getb().tobytearray(&mut point_bytes, 48);
    point.getpy().geta().tobytearray(&mut point_bytes, 96);
    point.getpy().getb().tobytearray(&mut point_bytes, 144);
    point.getpz().geta().tobytearray(&mut point_bytes, 192);
    point.getpz().getb().tobytearray(&mut point_bytes, 240);

    // Convert the output to one set of bytes (x.a, x.b, y.a, y.b, z.a, z.b)
    let mut decoded: Vec<u8> = vec![];
    for coordinate in output {
        let mut decoded_part = hex::decode(&coordinate[0][2..])
            .map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))?;
        decoded.append(&mut decoded_part);
        decoded_part = hex::decode(&coordinate[1][2..])
            .map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))?;
        decoded.append(&mut decoded_part);
    }

    compare_result::<Vec<u8>, Vec<u8>>(&Ok(point_bytes.to_vec()), &Some(decoded))
}

// Converts a vector to u64 (from big endian)
fn bytes_to_u64(array: &Vec<u8>) -> u64 {
    let mut result: u64 = 0;
    for (i, value) in array.iter().rev().enumerate() {
        if i == 8 {
            break;
        }
        result += u64::pow(2, i as u32 * 8) * (*value as u64);
    }
    result
}
