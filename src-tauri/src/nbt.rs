//! Little-endian NBT, as used by Minecraft Bedrock (level.dat body, block palettes, player data).
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub enum Tag {
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    ByteArray(Vec<u8>),
    Str(String),
    List(Vec<Tag>),
    Compound(HashMap<String, Tag>),
    IntArray(Vec<i32>),
    LongArray(Vec<i64>),
}

impl Tag {
    pub fn get(&self, key: &str) -> Option<&Tag> {
        match self {
            Tag::Compound(m) => m.get(key),
            _ => None,
        }
    }
    /// Any numeric tag as f64 (like `typeof x === 'number'` in JavaScript).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Tag::Byte(v) => Some(*v as f64),
            Tag::Short(v) => Some(*v as f64),
            Tag::Int(v) => Some(*v as f64),
            Tag::Long(v) => Some(*v as f64),
            Tag::Float(v) => Some(*v as f64),
            Tag::Double(v) => Some(*v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Tag::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&Vec<Tag>> {
        match self {
            Tag::List(l) => Some(l),
            _ => None,
        }
    }
}

pub struct Parsed {
    pub name: String,
    pub value: Tag,
    pub end: usize,
}

struct Reader<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.p.checked_add(n).ok_or("NBT length overflow")?;
        if end > self.b.len() {
            return Err("Truncated NBT data.".into());
        }
        let s = &self.b[self.p..end];
        self.p = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64, String> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn string(&mut self) -> Result<String, String> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8_lossy(self.take(n)?).into_owned())
    }
    fn len(&mut self) -> Result<usize, String> {
        let n = self.i32()?;
        if n < 0 {
            return Err("Negative NBT length.".into());
        }
        Ok(n as usize)
    }
}

fn read_tag(r: &mut Reader, ty: u8, depth: u32) -> Result<Tag, String> {
    if depth > 64 {
        return Err("NBT nested too deeply.".into());
    }
    Ok(match ty {
        1 => Tag::Byte(r.u8()? as i8),
        2 => Tag::Short(r.i16()?),
        3 => Tag::Int(r.i32()?),
        4 => Tag::Long(r.i64()?),
        5 => Tag::Float(r.f32()?),
        6 => Tag::Double(r.f64()?),
        7 => {
            let n = r.len()?;
            Tag::ByteArray(r.take(n)?.to_vec())
        }
        8 => Tag::Str(r.string()?),
        9 => {
            let t = r.u8()?;
            let n = r.len()?;
            let mut out = Vec::new();
            for _ in 0..n {
                out.push(read_tag(r, t, depth + 1)?);
            }
            Tag::List(out)
        }
        10 => {
            let mut m = HashMap::new();
            loop {
                // A missing end byte at the very end of the data is tolerated, as before.
                let t = match r.b.get(r.p) {
                    Some(&t) => {
                        r.p += 1;
                        t
                    }
                    None => break,
                };
                if t == 0 {
                    break;
                }
                let name = r.string()?;
                m.insert(name, read_tag(r, t, depth + 1)?);
            }
            Tag::Compound(m)
        }
        11 => {
            let n = r.len()?;
            let mut a = Vec::new();
            for _ in 0..n {
                a.push(r.i32()?);
            }
            Tag::IntArray(a)
        }
        12 => {
            let n = r.len()?;
            let mut a = Vec::new();
            for _ in 0..n {
                a.push(r.i64()?);
            }
            Tag::LongArray(a)
        }
        _ => return Err(format!("Unknown NBT tag type {}", ty)),
    })
}

/// Parses one named root tag starting at `pos`. Returns None for an end tag / no data.
pub fn parse(buf: &[u8], pos: usize) -> Result<Option<Parsed>, String> {
    let mut r = Reader { b: buf, p: pos };
    let ty = match buf.get(pos) {
        Some(&t) => t,
        None => return Ok(None),
    };
    r.p += 1;
    if ty == 0 {
        return Ok(None);
    }
    let name = r.string()?;
    let value = read_tag(&mut r, ty, 0)?;
    Ok(Some(Parsed { name, value, end: r.p }))
}
