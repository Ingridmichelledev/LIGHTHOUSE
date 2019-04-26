use super::*;
use crate::merkleize::{merkleize, pad_for_leaf_count};
use int_to_bytes::int_to_bytes32;

#[derive(Debug, PartialEq, Clone)]
pub struct BTreeSchema {
    pub depth: usize,
    pub lengths: Vec<usize>,
}

impl BTreeSchema {
    pub fn into_overlay(self, offset: usize) -> BTreeOverlay {
        BTreeOverlay {
            offset,
            depth: self.depth,
            lengths: self.lengths,
        }
    }
}

impl Into<BTreeSchema> for BTreeOverlay {
    fn into(self) -> BTreeSchema {
        BTreeSchema {
            depth: self.depth,
            lengths: self.lengths,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct TreeHashCache {
    pub cache: Vec<u8>,
    pub chunk_modified: Vec<bool>,
    pub schemas: Vec<BTreeSchema>,

    pub chunk_index: usize,
    pub schema_index: usize,
}

impl Into<Vec<u8>> for TreeHashCache {
    fn into(self) -> Vec<u8> {
        self.cache
    }
}

impl TreeHashCache {
    pub fn new<T>(item: &T, depth: usize) -> Result<Self, Error>
    where
        T: CachedTreeHash<T>,
    {
        item.new_tree_hash_cache(depth)
    }

    pub fn from_leaves_and_subtrees<T>(
        item: &T,
        leaves_and_subtrees: Vec<Self>,
        depth: usize,
    ) -> Result<Self, Error>
    where
        T: CachedTreeHash<T>,
    {
        let overlay = BTreeOverlay::new(item, 0, depth);

        // Note how many leaves were provided. If is not a power-of-two, we'll need to pad it out
        // later.
        let num_provided_leaf_nodes = leaves_and_subtrees.len();

        // Allocate enough bytes to store the internal nodes and the leaves and subtrees, then fill
        // all the to-be-built internal nodes with zeros and append the leaves and subtrees.
        let internal_node_bytes = overlay.num_internal_nodes() * BYTES_PER_CHUNK;
        let leaves_and_subtrees_bytes = leaves_and_subtrees
            .iter()
            .fold(0, |acc, t| acc + t.bytes_len());
        let mut cache = Vec::with_capacity(leaves_and_subtrees_bytes + internal_node_bytes);
        cache.resize(internal_node_bytes, 0);

        // Allocate enough bytes to store all the leaves.
        let mut leaves = Vec::with_capacity(overlay.num_leaf_nodes() * HASHSIZE);
        let mut schemas = Vec::with_capacity(leaves_and_subtrees.len());

        if T::tree_hash_type() == TreeHashType::List {
            schemas.push(overlay.into());
        }

        // Iterate through all of the leaves/subtrees, adding their root as a leaf node and then
        // concatenating their merkle trees.
        for t in leaves_and_subtrees {
            leaves.append(&mut t.root()?.to_vec());

            let (mut bytes, _bools, mut t_schemas) = t.into_components();
            cache.append(&mut bytes);
            schemas.append(&mut t_schemas);
        }

        // Pad the leaves to an even power-of-two, using zeros.
        pad_for_leaf_count(num_provided_leaf_nodes, &mut cache);

        // Merkleize the leaves, then split the leaf nodes off them. Then, replace all-zeros
        // internal nodes created earlier with the internal nodes generated by `merkleize`.
        let mut merkleized = merkleize(leaves);
        merkleized.split_off(internal_node_bytes);
        cache.splice(0..internal_node_bytes, merkleized);

        Ok(Self {
            chunk_modified: vec![false; cache.len() / BYTES_PER_CHUNK],
            cache,
            schemas,
            chunk_index: 0,
            schema_index: 0,
        })
    }

    pub fn from_bytes(
        bytes: Vec<u8>,
        initial_modified_state: bool,
        overlay: Option<BTreeOverlay>,
    ) -> Result<Self, Error> {
        if bytes.len() % BYTES_PER_CHUNK > 0 {
            return Err(Error::BytesAreNotEvenChunks(bytes.len()));
        }

        let schemas = match overlay {
            Some(overlay) => vec![overlay.into()],
            None => vec![],
        };

        Ok(Self {
            chunk_modified: vec![initial_modified_state; bytes.len() / BYTES_PER_CHUNK],
            cache: bytes,
            schemas,
            chunk_index: 0,
            schema_index: 0,
        })
    }

    pub fn get_overlay(
        &self,
        schema_index: usize,
        chunk_index: usize,
    ) -> Result<BTreeOverlay, Error> {
        Ok(self
            .schemas
            .get(schema_index)
            .ok_or_else(|| Error::NoSchemaForIndex(schema_index))?
            .clone()
            .into_overlay(chunk_index))
    }

    pub fn reset_modifications(&mut self) {
        for chunk_modified in &mut self.chunk_modified {
            *chunk_modified = false;
        }
    }

    pub fn replace_overlay(
        &mut self,
        schema_index: usize,
        chunk_index: usize,
        new_overlay: BTreeOverlay,
    ) -> Result<BTreeOverlay, Error> {
        let old_overlay = self.get_overlay(schema_index, chunk_index)?;

        // If the merkle tree required to represent the new list is of a different size to the one
        // required for the previous list, then update our cache.
        //
        // This grows/shrinks the bytes to accomodate the new tree, preserving as much of the tree
        // as possible.
        if new_overlay.num_leaf_nodes() != old_overlay.num_leaf_nodes() {
            // Get slices of the exsiting tree from the cache.
            let (old_bytes, old_flags) = self
                .slices(old_overlay.chunk_range())
                .ok_or_else(|| Error::UnableToObtainSlices)?;

            let (new_bytes, new_bools) =
                if new_overlay.num_leaf_nodes() > old_overlay.num_leaf_nodes() {
                    resize::grow_merkle_cache(
                        old_bytes,
                        old_flags,
                        old_overlay.height(),
                        new_overlay.height(),
                    )
                    .ok_or_else(|| Error::UnableToGrowMerkleTree)?
                } else {
                    resize::shrink_merkle_cache(
                        old_bytes,
                        old_flags,
                        old_overlay.height(),
                        new_overlay.height(),
                        new_overlay.num_chunks(),
                    )
                    .ok_or_else(|| Error::UnableToShrinkMerkleTree)?
                };

            // Splice the newly created `TreeHashCache` over the existing elements.
            self.splice(old_overlay.chunk_range(), new_bytes, new_bools);
        }

        let old_schema = std::mem::replace(&mut self.schemas[schema_index], new_overlay.into());

        Ok(old_schema.into_overlay(chunk_index))
    }

    pub fn remove_proceeding_child_schemas(&mut self, schema_index: usize, depth: usize) {
        let end = self
            .schemas
            .iter()
            .skip(schema_index)
            .position(|o| o.depth <= depth)
            .and_then(|i| Some(i + schema_index))
            .unwrap_or_else(|| self.schemas.len());

        self.schemas.splice(schema_index..end, vec![]);
    }

    pub fn update_internal_nodes(&mut self, overlay: &BTreeOverlay) -> Result<(), Error> {
        for (parent, children) in overlay.internal_parents_and_children().into_iter().rev() {
            if self.either_modified(children)? {
                self.modify_chunk(parent, &self.hash_children(children)?)?;
            }
        }

        Ok(())
    }

    fn bytes_len(&self) -> usize {
        self.cache.len()
    }

    pub fn root(&self) -> Result<&[u8], Error> {
        self.cache
            .get(0..HASHSIZE)
            .ok_or_else(|| Error::NoBytesForRoot)
    }

    pub fn splice(&mut self, chunk_range: Range<usize>, bytes: Vec<u8>, bools: Vec<bool>) {
        // Update the `chunk_modified` vec, marking all spliced-in nodes as changed.
        self.chunk_modified.splice(chunk_range.clone(), bools);
        self.cache
            .splice(node_range_to_byte_range(&chunk_range), bytes);
    }

    pub fn maybe_update_chunk(&mut self, chunk: usize, to: &[u8]) -> Result<(), Error> {
        let start = chunk * BYTES_PER_CHUNK;
        let end = start + BYTES_PER_CHUNK;

        if !self.chunk_equals(chunk, to)? {
            self.cache
                .get_mut(start..end)
                .ok_or_else(|| Error::NoModifiedFieldForChunk(chunk))?
                .copy_from_slice(to);
            self.chunk_modified[chunk] = true;
        }

        Ok(())
    }

    fn slices(&self, chunk_range: Range<usize>) -> Option<(&[u8], &[bool])> {
        Some((
            self.cache.get(node_range_to_byte_range(&chunk_range))?,
            self.chunk_modified.get(chunk_range)?,
        ))
    }

    pub fn modify_chunk(&mut self, chunk: usize, to: &[u8]) -> Result<(), Error> {
        let start = chunk * BYTES_PER_CHUNK;
        let end = start + BYTES_PER_CHUNK;

        self.cache
            .get_mut(start..end)
            .ok_or_else(|| Error::NoBytesForChunk(chunk))?
            .copy_from_slice(to);

        self.chunk_modified[chunk] = true;

        Ok(())
    }

    fn get_chunk(&self, chunk: usize) -> Result<&[u8], Error> {
        let start = chunk * BYTES_PER_CHUNK;
        let end = start + BYTES_PER_CHUNK;

        Ok(self
            .cache
            .get(start..end)
            .ok_or_else(|| Error::NoModifiedFieldForChunk(chunk))?)
    }

    fn chunk_equals(&mut self, chunk: usize, other: &[u8]) -> Result<bool, Error> {
        Ok(self.get_chunk(chunk)? == other)
    }

    pub fn changed(&self, chunk: usize) -> Result<bool, Error> {
        self.chunk_modified
            .get(chunk)
            .cloned()
            .ok_or_else(|| Error::NoModifiedFieldForChunk(chunk))
    }

    fn either_modified(&self, children: (usize, usize)) -> Result<bool, Error> {
        Ok(self.changed(children.0)? | self.changed(children.1)?)
    }

    pub fn hash_children(&self, children: (usize, usize)) -> Result<Vec<u8>, Error> {
        let mut child_bytes = Vec::with_capacity(BYTES_PER_CHUNK * 2);
        child_bytes.append(&mut self.get_chunk(children.0)?.to_vec());
        child_bytes.append(&mut self.get_chunk(children.1)?.to_vec());

        Ok(hash(&child_bytes))
    }

    pub fn add_length_nodes(
        &mut self,
        chunk_range: Range<usize>,
        length: usize,
    ) -> Result<(), Error> {
        self.chunk_modified[chunk_range.start] = true;

        let byte_range = node_range_to_byte_range(&chunk_range);

        // Add the last node.
        self.cache
            .splice(byte_range.end..byte_range.end, vec![0; HASHSIZE]);
        self.chunk_modified
            .splice(chunk_range.end..chunk_range.end, vec![false]);

        // Add the first node.
        self.cache
            .splice(byte_range.start..byte_range.start, vec![0; HASHSIZE]);
        self.chunk_modified
            .splice(chunk_range.start..chunk_range.start, vec![false]);

        self.mix_in_length(chunk_range.start + 1..chunk_range.end + 1, length)?;

        Ok(())
    }

    pub fn mix_in_length(&mut self, chunk_range: Range<usize>, length: usize) -> Result<(), Error> {
        // Update the length chunk.
        self.maybe_update_chunk(chunk_range.end, &int_to_bytes32(length as u64))?;

        // Update the mixed-in root if the main root or the length have changed.
        let children = (chunk_range.start, chunk_range.end);
        if self.either_modified(children)? {
            self.modify_chunk(chunk_range.start - 1, &self.hash_children(children)?)?;
        }

        Ok(())
    }

    pub fn into_components(self) -> (Vec<u8>, Vec<bool>, Vec<BTreeSchema>) {
        (self.cache, self.chunk_modified, self.schemas)
    }
}

fn node_range_to_byte_range(node_range: &Range<usize>) -> Range<usize> {
    node_range.start * HASHSIZE..node_range.end * HASHSIZE
}
