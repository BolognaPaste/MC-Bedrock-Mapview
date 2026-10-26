use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

/// Writes an 8-bit RGBA PNG.
pub fn write(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())
}

/// Reads an 8-bit RGBA PNG written by `write`.
pub fn read(path: &Path) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut reader = png::Decoder::new(file).read_info().map_err(|e| e.to_string())?;
    let info = reader.info();
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err("Unsupported PNG.".into());
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(frame.buffer_size());
    Ok(buf)
}
