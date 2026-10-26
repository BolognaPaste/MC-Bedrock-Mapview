//! Decodes the block storages of a Bedrock SubChunk record.
//! Storage 0 holds the blocks. Storage 1, when present, holds what shares a block's space,
//! which in practice is the water inside waterlogged blocks (kelp, seagrass, waterlogged slabs...).
use crate::nbt;

/// One palettised block storage.
pub struct Storage {
    pub palette: Vec<String>,
    /// idx[x*256 + z*16 + y] is a palette index.
    pub idx: Vec<u16>,
}

pub struct SubChunk {
    pub palette: Vec<String>,
    /// idx[x*256 + z*16 + y] is a palette index.
    pub idx: Vec<u16>,
    /// The second storage (mostly water in waterlogged blocks), if the record has one and it could be read.
    pub layer1: Option<Storage>,
}

fn decode_storage(buf: &[u8], p: &mut usize) -> Result<Storage, String> {
    let header = *buf.get(*p).ok_or("Truncated subchunk.")?;
    *p += 1;
    if header & 1 != 0 {
        return Err("Network-format palette in saved data.".into());
    }
    let bpb = (header >> 1) as usize;
    // Only these sizes exist. Anything else means damaged data, and must not reach the
    // division and shift below (bpb > 32 divides by zero, bpb == 32 overflows the shift).
    if !matches!(bpb, 0 | 1 | 2 | 3 | 4 | 5 | 6 | 8 | 16) {
        return Err(format!("Bad bits-per-block value {}", bpb));
    }
    let mut idx = vec![0u16; 4096];
    if bpb > 0 {
        let per = 32 / bpb;
        let words = (4096 + per - 1) / per;
        let mask = (1u32 << bpb) - 1;
        for w in 0..words {
            let s = buf.get(*p + w * 4..*p + w * 4 + 4).ok_or("Truncated subchunk.")?;
            let word = u32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            let mut i = w * per;
            let mut j = 0;
            while j < per && i < 4096 {
                idx[i] = ((word >> (j * bpb)) & mask) as u16;
                j += 1;
                i += 1;
            }
        }
        *p += words * 4;
    }
    let s = buf.get(*p..*p + 4).ok_or("Truncated subchunk.")?;
    let count = i32::from_le_bytes([s[0], s[1], s[2], s[3]]);
    *p += 4;
    if !(0..=4096).contains(&count) {
        return Err("Bad palette size.".into());
    }
    let mut palette = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let t = nbt::parse(buf, *p)?.ok_or("Bad palette entry.")?;
        let name = t.value.get("name").and_then(|v| v.as_str()).unwrap_or("minecraft:air");
        palette.push(name.to_string());
        *p = t.end;
    }
    Ok(Storage { palette, idx })
}

pub fn decode_subchunk(buf: &[u8]) -> Result<Option<SubChunk>, String> {
    let get = |p: usize| buf.get(p).copied().ok_or_else(|| "Truncated subchunk.".to_string());
    let mut p = 0usize;
    let version = get(p)?;
    p += 1;
    if version != 1 && version != 8 && version != 9 {
        return Err(format!("Unsupported subchunk version {}", version));
    }
    let mut storages = 1u8;
    if version >= 8 {
        storages = get(p)?;
        p += 1;
        if version == 9 {
            p += 1;
        }
    }
    if storages < 1 {
        return Ok(None);
    }
    let first = decode_storage(buf, &mut p)?;
    // A damaged second storage only costs the water-in-blocks detail, not the whole sub-chunk.
    let layer1 = if storages >= 2 { decode_storage(buf, &mut p).ok() } else { None };
    Ok(Some(SubChunk { palette: first.palette, idx: first.idx, layer1 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_bits_per_block_is_an_error_not_a_panic() {
        // version 8, one storage, then a header whose bits-per-block is 33, 32 and 7
        for bpb in [33u8, 32, 7, 127] {
            let buf = [8u8, 1, bpb << 1];
            assert!(decode_subchunk(&buf).is_err());
        }
    }

    #[test]
    fn single_block_subchunk_decodes() {
        // version 8, one storage, bpb 0, palette of one "minecraft:stone"
        let mut buf = vec![8u8, 1, 0];
        buf.extend_from_slice(&1i32.to_le_bytes());
        buf.push(10); // compound
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.push(8); // string tag "name"
        buf.extend_from_slice(&4u16.to_le_bytes());
        buf.extend_from_slice(b"name");
        let v = b"minecraft:stone";
        buf.extend_from_slice(&(v.len() as u16).to_le_bytes());
        buf.extend_from_slice(v);
        buf.push(0);
        let sc = decode_subchunk(&buf).unwrap().unwrap();
        assert_eq!(sc.palette, vec!["minecraft:stone".to_string()]);
        assert!(sc.idx.iter().all(|&i| i == 0));
    }

    fn storage_bytes(bpb0_name: &str) -> Vec<u8> {
        // bpb 0 storage: header, palette count 1, one compound {name: <bpb0_name>}
        let mut b = vec![0u8];
        b.extend_from_slice(&1i32.to_le_bytes());
        b.push(10);
        b.extend_from_slice(&0u16.to_le_bytes());
        b.push(8);
        b.extend_from_slice(&4u16.to_le_bytes());
        b.extend_from_slice(b"name");
        b.extend_from_slice(&(bpb0_name.len() as u16).to_le_bytes());
        b.extend_from_slice(bpb0_name.as_bytes());
        b.push(0);
        b
    }

    #[test]
    fn second_storage_is_read() {
        let mut buf = vec![8u8, 2];
        buf.extend(storage_bytes("minecraft:kelp"));
        buf.extend(storage_bytes("minecraft:water"));
        let sc = decode_subchunk(&buf).unwrap().unwrap();
        assert_eq!(sc.palette, vec!["minecraft:kelp".to_string()]);
        let l1 = sc.layer1.expect("layer 1");
        assert_eq!(l1.palette, vec!["minecraft:water".to_string()]);
    }

    #[test]
    fn broken_second_storage_keeps_the_first() {
        let mut buf = vec![8u8, 2];
        buf.extend(storage_bytes("minecraft:stone"));
        buf.push(7 << 1); // invalid bits-per-block in storage 1
        let sc = decode_subchunk(&buf).unwrap().unwrap();
        assert_eq!(sc.palette, vec!["minecraft:stone".to_string()]);
        assert!(sc.layer1.is_none());
    }
}
