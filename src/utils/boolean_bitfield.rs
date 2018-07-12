/*
 * Implemenation of a bitfield as a vec. Only
 * supports bytes (Vec<u8>) as the underlying
 * storage.
 *
 * A future implementation should be more efficient,
 * this is just to get the job done for now.
 */
extern crate rlp;
use self::rlp::{ RlpStream, Encodable };

pub struct BooleanBitfield{
    vec: Vec<u8>
}

impl BooleanBitfield {
    pub fn new() -> Self {
        Self {
            vec: vec![]
        }
    }
    
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            vec: Vec::with_capacity(capacity)
        }
    }

    // Output the bitfield as a big-endian vec of u8
    pub fn to_be_vec(&self) -> Vec<u8> {
        let mut o = self.vec.clone();
        o.reverse();
        o
    }

    pub fn get_bit(&self, i: &usize) -> bool {
        self.get_bit_on_byte(*i % 8, *i / 8)
    }

    fn get_bit_on_byte(&self, bit: usize, byte: usize) -> bool {
        assert!(bit < 8);
        if byte >= self.vec.len() {
            false
        } else {
             self.vec[byte] & (1 << (bit as u8)) != 0
        }
    }

    pub fn set_bit(&mut self, bit: &usize, to: &bool) {
        self.set_bit_on_byte(*bit % 8, *bit / 8, to);
    }

    fn set_bit_on_byte(&mut self, bit: usize, byte: usize, val: &bool) {
        assert!(bit < 8);
        if byte >= self.vec.len() {
            self.vec.resize(byte + 1, 0);
        }
        match val {
            true =>  self.vec[byte] = self.vec[byte] |  (1 << (bit as u8)),
            false => self.vec[byte] = self.vec[byte] & !(1 << (bit as u8))
        }
    }
}

impl Encodable for BooleanBitfield {
    // TODO: ensure this is a sensible method of encoding
    // the bitfield. Currently, it is treated as a list of 
    // bytes not as a string. I do not have any guidance as 
    // to which method is correct -- don't follow my lead
    // without seeking authoritative advice.
    fn rlp_append(&self, s: &mut RlpStream) {
        s.append(&self.to_be_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::rlp;

    #[test]
    fn test_bitfield_set() {
        let mut b = BooleanBitfield::new();
        b.set_bit(&0, &false);
        assert_eq!(b.to_be_vec(), [0]);
        
        b = BooleanBitfield::new();
        b.set_bit(&7, &true);
        assert_eq!(b.to_be_vec(), [128]);
        b.set_bit(&7, &false);
        assert_eq!(b.to_be_vec(), [0]);
        
        b = BooleanBitfield::new();
        b.set_bit(&7, &true);
        b.set_bit(&0, &true);
        assert_eq!(b.to_be_vec(), [129]);
        b.set_bit(&7, &false);
        assert_eq!(b.to_be_vec(), [1]);
        
        b = BooleanBitfield::new();
        b.set_bit(&8, &true);
        assert_eq!(b.to_be_vec(), [1, 0]);
        b.set_bit(&8, &false);
        assert_eq!(b.to_be_vec(), [0, 0]);
        
        b = BooleanBitfield::new();
        b.set_bit(&15, &true);
        assert_eq!(b.to_be_vec(), [128, 0]);
        b.set_bit(&15, &false);
        assert_eq!(b.to_be_vec(), [0, 0]);
        
        b = BooleanBitfield::new();
        b.set_bit(&8, &true);
        b.set_bit(&15, &true);
        assert_eq!(b.to_be_vec(), [129, 0]);
        b.set_bit(&15, &false);
        assert_eq!(b.to_be_vec(), [1, 0]);
    }

    #[test]
    fn test_bitfield_get() {
        let test_nums = vec![0, 8, 15, 42, 1337];
        for i in test_nums {
            let mut b = BooleanBitfield::new();
            assert_eq!(b.get_bit(&i), false);
            b.set_bit(&i, &true);
            assert_eq!(b.get_bit(&i), true);
            b.set_bit(&i, &true);
        } 
    }
    
    #[test]
    fn test_bitfield_rlp_serialization() {
        let mut b = BooleanBitfield::new();
        b.set_bit(&15, &true);
        let e = rlp::encode(&b);
        assert_eq!(e[0], 130);
        assert_eq!(e[1], 128);
        assert_eq!(e[2], 0);
    }
}
