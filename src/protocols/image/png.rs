use crate::prelude::*;

pub fn encode_png_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    use png::{BitDepth, ColorType, Encoder};

    let mut buf = Vec::new();
    {
        let mut encoder = Encoder::new(&mut buf, width, height);
        encoder.set_color(ColorType::Rgba);
        encoder.set_depth(BitDepth::Eight);
        let mut writer = encoder.write_header().location(loc!())?;
        writer.write_image_data(rgba).location(loc!())?;
        writer.finish().location(loc!())?;
    }
    Ok(buf)
}
