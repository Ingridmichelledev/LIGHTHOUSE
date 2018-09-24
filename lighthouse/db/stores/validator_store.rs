extern crate bytes;

use self::bytes::{
    BufMut,
    BytesMut,
};
use std::sync::Arc;
use super::{
    ClientDB,
    StoreError,
};
use super::VALIDATOR_DB_COLUMN as DB_COLUMN;
use super::bls::PublicKey;

#[derive(Debug, PartialEq)]
enum KeyPrefixes {
    PublicKey,
}

pub struct ValidatorStore<T>
    where T: ClientDB
{
    db: Arc<T>,
}

impl<T: ClientDB> ValidatorStore<T> {
    pub fn new(db: Arc<T>) -> Self {
        Self {
            db,
        }
    }

    fn prefix_bytes(&self, key_prefix: KeyPrefixes)
        -> Vec<u8>
    {
        match key_prefix {
            KeyPrefixes::PublicKey => b"pubkey".to_vec(),
        }
    }

    fn get_db_key_for_index(&self, key_prefix: KeyPrefixes, index: usize)
        -> Vec<u8>
    {
        let mut buf = BytesMut::with_capacity(6 + 8);
        buf.put(self.prefix_bytes(key_prefix));
        buf.put_u64_be(index as u64);
        buf.take().to_vec()
    }

    pub fn put_public_key_by_index(&self, index: usize, public_key: &PublicKey)
        -> Result<(), StoreError>
    {
        let key = self.get_db_key_for_index(KeyPrefixes::PublicKey, index);
        let val = public_key.as_bytes();
        self.db.put(DB_COLUMN, &key[..], &val[..])
                    .map_err(|e| StoreError::from(e))
    }

    pub fn get_public_key_by_index(&self, index: usize)
        -> Result<Option<PublicKey>, StoreError>
    {
        let key = self.get_db_key_for_index(KeyPrefixes::PublicKey, index);
        let val = self.db.get(DB_COLUMN, &key[..])?;
        match val {
            None => Ok(None),
            Some(val) => {
                match PublicKey::from_bytes(&val) {
                    Ok(key) => Ok(Some(key)),
                    Err(_) => Err(StoreError::DecodeError),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::super::{
        MemoryDB,
        ClientDB,
    };
    use super::super::bls::Keypair;

    fn open_client_db() -> MemoryDB {
        let columns = vec![DB_COLUMN];
        MemoryDB::open(Some(&columns))
    }

    #[test]
    fn test_validator_store_put_get() {
        let db = Arc::new(open_client_db());
        let store = ValidatorStore::new(db);

        let keys = vec![
            Keypair::random(),
            Keypair::random(),
            Keypair::random(),
            Keypair::random(),
            Keypair::random(),
        ];

        for i in 0..keys.len() {
            store.put_public_key_by_index(i, &keys[i].pk).unwrap();
        }

        /*
         * Check all keys are retrieved correctly.
         */
        for i in 0..keys.len() {
            let retrieved = store.get_public_key_by_index(i)
                .unwrap().unwrap();
            assert_eq!(retrieved, keys[i].pk);
        }

        /*
         * Check that an index that wasn't stored returns None.
         */
        assert!(store.get_public_key_by_index(keys.len() + 1)
                .unwrap().is_none());
    }

    #[test]
    fn test_validator_store_bad_key() {
        let db = Arc::new(open_client_db());
        let store = ValidatorStore::new(db.clone());

        let key = store.get_db_key_for_index(KeyPrefixes::PublicKey, 42);
        db.put(DB_COLUMN, &key[..], "cats".as_bytes()).unwrap();

        assert_eq!(store.get_public_key_by_index(42),
            Err(StoreError::DecodeError));
    }
}
