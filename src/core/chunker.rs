use crate::core::errors::ChunkError;
use crate::core::hash::sha256_hex;
use fastcdc::v2020::StreamCDC;
use serde::{Deserialize, Serialize};
use std::io::Read;

pub const CHUNKER_ALGO: &str = "fastcdc-v2020";
pub const CHUNK_MIN_SIZE: u32 = 16384;
pub const CHUNK_AVG_SIZE: u32 = 65536;
pub const CHUNK_MAX_SIZE: u32 = 262144;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkInfo {
    pub hash: String,
    pub len: u64,
    pub offset: u64,
}

pub fn chunk_reader<R: Read>(reader: R) -> Result<Vec<ChunkInfo>, ChunkError> {
    let chunker = StreamCDC::new(reader, CHUNK_MIN_SIZE, CHUNK_AVG_SIZE, CHUNK_MAX_SIZE);
    let mut chunks = Vec::new();

    for entry in chunker {
        let chunk = entry?;
        let hash = sha256_hex(&chunk.data);
        chunks.push(ChunkInfo {
            hash,
            len: chunk.length as u64,
            offset: chunk.offset,
        });
    }

    Ok(chunks)
}

pub fn chunk_slice(data: &[u8]) -> Result<Vec<ChunkInfo>, ChunkError> {
    chunk_reader(data)
}
