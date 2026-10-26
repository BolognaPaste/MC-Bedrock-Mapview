//! Minimal read-only LevelDB reader for Minecraft Bedrock worlds.
//! Reads every .ldb/.sst table and .log file in a folder and keeps the newest value for each key.
//! Bedrock's LevelDB fork adds zlib block compression (types 2 and 4) on top of the standard formats.
//! After `open`, a LevelDb can be shared between threads: file reads use positional reads (no shared
//! file cursor) and the block cache sits behind a mutex.
use flate2::read::{DeflateDecoder, ZlibDecoder};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// How many decompressed table blocks to keep. Several threads draw neighbouring tiles at once,
/// so this is larger than a single-threaded reader would need.
const CACHE_BLOCKS: usize = 512;

const MAGIC: [u8; 8] = [0x57, 0xfb, 0x80, 0x8b, 0x24, 0x75, 0x47, 0xdb];

type R<T> = Result<T, String>;

pub fn varint(buf: &[u8], mut pos: usize) -> R<(u64, usize)> {
    let mut r: u64 = 0;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(pos).ok_or("Truncated varint.")?;
        pos += 1;
        if shift < 64 {
            r |= ((b & 127) as u64) << shift;
        }
        shift += 7;
        if b & 128 == 0 {
            break;
        }
    }
    Ok((r, pos))
}

pub fn snappy(src: &[u8]) -> R<Vec<u8>> {
    let (len, mut p) = varint(src, 0)?;
    let len = len as usize;
    let mut out: Vec<u8> = Vec::with_capacity(len);
    while p < src.len() {
        let t = src[p];
        p += 1;
        let ty = t & 3;
        if ty == 0 {
            let mut l = (t >> 2) as usize;
            if l < 60 {
                l += 1;
            } else {
                let n = l - 59;
                l = 0;
                for i in 0..n {
                    l += (*src.get(p).ok_or("Truncated snappy data.")? as usize) << (8 * i);
                    p += 1;
                }
                l += 1;
            }
            let end = p.checked_add(l).ok_or("Bad snappy length.")?;
            if end > src.len() {
                return Err("Truncated snappy data.".into());
            }
            out.extend_from_slice(&src[p..end]);
            p = end;
        } else {
            let (l, off);
            if ty == 1 {
                l = (((t >> 2) & 7) + 4) as usize;
                off = (((t >> 5) as usize) << 8) | *src.get(p).ok_or("Truncated snappy data.")? as usize;
                p += 1;
            } else if ty == 2 {
                l = (t >> 2) as usize + 1;
                let b = src.get(p..p + 2).ok_or("Truncated snappy data.")?;
                off = u16::from_le_bytes([b[0], b[1]]) as usize;
                p += 2;
            } else {
                l = (t >> 2) as usize + 1;
                let b = src.get(p..p + 4).ok_or("Truncated snappy data.")?;
                off = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize;
                p += 4;
            }
            if off == 0 || off > out.len() {
                return Err("Bad snappy offset.".into());
            }
            for _ in 0..l {
                let v = out[out.len() - off];
                out.push(v);
            }
        }
    }
    Ok(out)
}

fn zlib(raw: &[u8]) -> R<Vec<u8>> {
    let mut v = Vec::new();
    ZlibDecoder::new(raw).read_to_end(&mut v).map_err(|e| e.to_string())?;
    Ok(v)
}
fn raw_deflate(raw: &[u8]) -> R<Vec<u8>> {
    let mut v = Vec::new();
    DeflateDecoder::new(raw).read_to_end(&mut v).map_err(|e| e.to_string())?;
    Ok(v)
}

fn decompress(raw: &[u8], ty: u8) -> R<Vec<u8>> {
    match ty {
        0 => Ok(raw.to_vec()),
        1 => snappy(raw),
        2 => zlib(raw).or_else(|_| raw_deflate(raw)),
        4 => raw_deflate(raw).or_else(|_| zlib(raw)),
        _ => Err(format!("Unknown block compression type {}", ty)),
    }
}

/// Reads exactly `buf.len()` bytes at `off` without using (or disturbing) a shared file position,
/// so several threads can read the same file at once.
fn read_at(file: &File, off: u64, buf: &mut [u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buf, off)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0usize;
        while done < buf.len() {
            let n = file.seek_read(&mut buf[done..], off + done as u64)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
}

fn read_block(file: &File, off: u64, size: usize) -> R<Vec<u8>> {
    let mut buf = vec![0u8; size + 5];
    read_at(file, off, &mut buf).map_err(|_| "Truncated table block.".to_string())?;
    decompress(&buf[..size], buf[size])
}

/// (key, value) pairs of one data or index block.
fn block_entries(block: &[u8]) -> R<Vec<(Vec<u8>, Vec<u8>)>> {
    if block.len() < 4 {
        return Err("Block too small.".into());
    }
    let restarts = u32::from_le_bytes(block[block.len() - 4..].try_into().unwrap()) as usize;
    let end = (block.len() - 4)
        .checked_sub(restarts.checked_mul(4).ok_or("Bad restart count.")?)
        .ok_or("Bad restart count.")?;
    let mut out = Vec::new();
    let mut p = 0usize;
    let mut prev: Vec<u8> = Vec::new();
    while p < end {
        let (shared, p1) = varint(block, p)?;
        let (non_shared, p2) = varint(block, p1)?;
        let (vlen, p3) = varint(block, p2)?;
        let (shared, non_shared, vlen) = (shared as usize, non_shared as usize, vlen as usize);
        if shared > prev.len() || p3 + non_shared + vlen > block.len() {
            return Err("Corrupt block entry.".into());
        }
        let mut key = prev[..shared].to_vec();
        key.extend_from_slice(&block[p3..p3 + non_shared]);
        let vs = p3 + non_shared;
        out.push((key.clone(), block[vs..vs + vlen].to_vec()));
        p = vs + vlen;
        prev = key;
    }
    Ok(out)
}

/// Where a value lives: inline (log files) or at an ordinal inside a table block.
#[derive(Clone)]
pub struct Loc {
    pub key: Vec<u8>,
    seq: u64,
    ty: u8,
    value: Option<Arc<Vec<u8>>>,
    fi: usize,
    off: u64,
    size: usize,
    ord: usize,
}

#[derive(Default)]
pub struct Stats {
    pub tables: u32,
    pub tables_failed: u32,
    pub blocks: u32,
    pub blocks_failed: u32,
    pub logs: u32,
}

/// Values of one decompressed table block, in entry order.
type Block = Arc<Vec<Vec<u8>>>;

#[derive(Default)]
struct BlockCache {
    map: HashMap<(usize, u64), Block>,
    order: VecDeque<(usize, u64)>,
}

pub struct LevelDb {
    filter: fn(&[u8]) -> bool,
    map: HashMap<Vec<u8>, Loc>,
    files: Vec<File>,
    cache: Mutex<BlockCache>,
    pub warnings: Vec<String>,
    pub stats: Stats,
}

impl LevelDb {
    pub fn open(dir: &Path, filter: fn(&[u8]) -> bool) -> R<LevelDb> {
        let mut db = LevelDb {
            filter,
            map: HashMap::new(),
            files: Vec::new(),
            cache: Mutex::new(BlockCache::default()),
            warnings: Vec::new(),
            stats: Stats::default(),
        };
        let mut tables = Vec::new();
        let mut logs = Vec::new();
        for e in fs::read_dir(dir).map_err(|e| e.to_string())? {
            let e = e.map_err(|e| e.to_string())?;
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if lower.ends_with(".ldb") || lower.ends_with(".sst") {
                tables.push(name);
            } else if lower.ends_with(".log") {
                logs.push(name);
            }
        }
        if tables.is_empty() && logs.is_empty() {
            return Err(format!("No LevelDB files found in {}.", dir.display()));
        }
        tables.sort();
        logs.sort();
        for n in &tables {
            match db.load_table(&dir.join(n)) {
                Ok(()) => db.stats.tables += 1,
                Err(e) => {
                    db.stats.tables_failed += 1;
                    db.warnings.push(format!("{}: {}", n, e));
                }
            }
        }
        for n in &logs {
            match db.load_log(&dir.join(n)) {
                Ok(()) => db.stats.logs += 1,
                Err(e) => db.warnings.push(format!("{}: {}", n, e)),
            }
        }
        db.map.retain(|_, v| v.ty != 0);
        Ok(db)
    }

    fn put(&mut self, key: &[u8], seq: u64, ty: u8, mut loc: Loc) {
        if !(self.filter)(key) {
            return;
        }
        if let Some(old) = self.map.get(key) {
            if old.seq >= seq {
                return;
            }
        }
        loc.seq = seq;
        loc.ty = ty;
        loc.key = key.to_vec();
        self.map.insert(key.to_vec(), loc);
    }

    fn blank(fi: usize, off: u64, size: usize, ord: usize) -> Loc {
        Loc { key: Vec::new(), seq: 0, ty: 0, value: None, fi, off, size, ord }
    }

    fn load_table(&mut self, file: &Path) -> R<()> {
        let f = File::open(file).map_err(|e| e.to_string())?;
        let fi = self.files.len();
        let size = f.metadata().map_err(|e| e.to_string())?.len();
        self.files.push(f);
        if size < 48 {
            return Err("Table too small.".into());
        }
        let mut footer = [0u8; 48];
        read_at(&self.files[fi], size - 48, &mut footer).map_err(|e| e.to_string())?;
        if footer[40..] != MAGIC {
            return Err("Not a LevelDB table.".into());
        }
        let (_mo, p) = varint(&footer, 0)?;
        let (_ms, p) = varint(&footer, p)?;
        let (io, p) = varint(&footer, p)?;
        let (is, _p) = varint(&footer, p)?;
        let index = block_entries(&read_block(&self.files[fi], io, is as usize)?)?;
        let fname = file.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        for (_, handle) in index {
            let (off, q) = varint(&handle, 0)?;
            let (len, _q) = varint(&handle, q)?;
            self.stats.blocks += 1;
            let entries = match read_block(&self.files[fi], off, len as usize).and_then(|b| block_entries(&b)) {
                Ok(e) => e,
                Err(e) => {
                    self.stats.blocks_failed += 1;
                    if self.warnings.len() < 20 {
                        self.warnings.push(format!("{} block at {}: {}", fname, off, e));
                    }
                    continue;
                }
            };
            for (ord, (ik, _)) in entries.iter().enumerate() {
                if ik.len() < 8 {
                    continue;
                }
                let n = ik.len();
                let lo = u32::from_le_bytes(ik[n - 8..n - 4].try_into().unwrap()) as u64;
                let hi = u32::from_le_bytes(ik[n - 4..].try_into().unwrap()) as u64;
                let seq = (hi << 24) | (lo >> 8);
                let loc = Self::blank(fi, off, len as usize, ord);
                self.put(&ik[..n - 8], seq, (lo & 255) as u8, loc);
            }
        }
        Ok(())
    }

    fn load_log(&mut self, file: &Path) -> R<()> {
        let data = fs::read(file).map_err(|e| e.to_string())?;
        let mut records: Vec<Vec<u8>> = Vec::new();
        let mut cur: Option<Vec<u8>> = None;
        let mut p = 0usize;
        while p + 7 <= data.len() {
            let block_left = 32768 - (p % 32768);
            if block_left < 7 {
                p += block_left;
                continue;
            }
            let len = u16::from_le_bytes([data[p + 4], data[p + 5]]) as usize;
            let ty = data[p + 6];
            p += 7;
            if ty == 0 && len == 0 {
                continue;
            }
            if p + len > data.len() {
                break;
            }
            let chunk = &data[p..p + len];
            p += len;
            match ty {
                1 => records.push(chunk.to_vec()),
                2 => cur = Some(chunk.to_vec()),
                3 => {
                    if let Some(c) = cur.as_mut() {
                        c.extend_from_slice(chunk);
                    }
                }
                4 => {
                    if let Some(mut c) = cur.take() {
                        c.extend_from_slice(chunk);
                        records.push(c);
                    }
                }
                _ => {}
            }
        }
        for rec in records {
            if rec.len() < 12 {
                continue;
            }
            let mut seq = u64::from_le_bytes(rec[0..8].try_into().unwrap());
            let count = u32::from_le_bytes(rec[8..12].try_into().unwrap());
            let mut q = 12usize;
            // A damaged tail of the log is ignored (the inner closure just stops).
            let _ = (|| -> R<()> {
                for _ in 0..count {
                    let t = *rec.get(q).ok_or("end")?;
                    q += 1;
                    let (kl, q1) = varint(&rec, q)?;
                    q = q1;
                    let key = rec.get(q..q + kl as usize).ok_or("end")?.to_vec();
                    q += kl as usize;
                    if t == 1 {
                        let (vl, q2) = varint(&rec, q)?;
                        q = q2;
                        let val = rec.get(q..q + vl as usize).ok_or("end")?.to_vec();
                        q += vl as usize;
                        let mut loc = Self::blank(0, 0, 0, 0);
                        loc.value = Some(Arc::new(val));
                        self.put(&key, seq, 1, loc);
                    } else {
                        self.put(&key, seq, 0, Self::blank(0, 0, 0, 0));
                    }
                    seq += 1;
                }
                Ok(())
            })();
        }
        Ok(())
    }

    fn block(&self, loc: &Loc) -> R<Block> {
        let id = (loc.fi, loc.off);
        if let Some(b) = self.cache.lock().unwrap().map.get(&id) {
            return Ok(b.clone());
        }
        // Read and decompress without holding the lock, so other threads are not kept waiting.
        // Two threads may occasionally decode the same block at once; the second insert is skipped.
        let entries = block_entries(&read_block(&self.files[loc.fi], loc.off, loc.size)?)?;
        let vals: Block = Arc::new(entries.into_iter().map(|(_, v)| v).collect());
        let mut cache = self.cache.lock().unwrap();
        if let Some(b) = cache.map.get(&id) {
            return Ok(b.clone());
        }
        cache.map.insert(id, vals.clone());
        cache.order.push_back(id);
        if cache.order.len() > CACHE_BLOCKS {
            if let Some(old) = cache.order.pop_front() {
                cache.map.remove(&old);
            }
        }
        Ok(vals)
    }

    /// Value for an index entry (as returned by entries()/lookup()).
    pub fn read(&self, loc: &Loc) -> R<Vec<u8>> {
        if let Some(v) = &loc.value {
            return Ok((**v).clone());
        }
        let b = self.block(loc)?;
        b.get(loc.ord).cloned().ok_or_else(|| "Value missing from block.".to_string())
    }

    pub fn lookup(&self, key: &[u8]) -> Option<&Loc> {
        self.map.get(key)
    }
    pub fn entries(&self) -> impl Iterator<Item = &Loc> {
        self.map.values()
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snappy_literal_and_copy() {
        // length 8: literal "abcd" then a copy of 4 bytes from offset 4 (type 1: len 4..11, 11-bit offset)
        let src = [8u8, 3 << 2, b'a', b'b', b'c', b'd', 0b000_000_01, 4];
        assert_eq!(snappy(&src).unwrap(), b"abcdabcd");
    }

    #[test]
    fn varints() {
        assert_eq!(varint(&[0x96, 0x01], 0).unwrap(), (150, 2));
        assert!(varint(&[0x80], 0).is_err());
    }

    #[test]
    fn leveldb_can_be_shared_between_threads() {
        fn assert_sync<T: Send + Sync>() {}
        assert_sync::<LevelDb>();
    }
}
