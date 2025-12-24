use crate::prelude::*;

pub fn encode_png_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    use png::{BitDepth, ColorType, Encoder};

    ensure!(
        width > 0 && height > 0,
        Error::InvalidArgument("png encode requires non-zero dimensions".to_string()),
    );

    let expected_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|v| v.checked_mul(4))
        .ok_or_else(|| Error::InvalidArgument("png encode: width/height overflow".to_string()))
        .location(loc!())?;

    ensure!(
        rgba.len() == expected_len,
        Error::InvalidArgument(format!(
            "png encode: rgba length mismatch (got {}, expected {})",
            rgba.len(),
            expected_len
        )),
    );

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_round_trip_rgba() -> Result<()> {
        let width = 16;
        let height = 8;

        let mut rgba = vec![0u8; width * height * 4];
        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) * 4;
                rgba[idx + 0] = (x * 13) as u8;
                rgba[idx + 1] = (y * 29) as u8;
                rgba[idx + 2] = ((x ^ y) * 7) as u8;
                rgba[idx + 3] = 255;
            }
        }

        let encoded = encode_png_rgba(&rgba, width as u32, height as u32).location(loc!())?;

        let decoder = png::Decoder::new(std::io::Cursor::new(encoded));
        let mut reader = decoder.read_info().location(loc!())?;
        let info = reader.info();
        ensure!(
            info.width == width as u32,
            Error::Internal("png decode: width mismatch".to_string()),
        );
        ensure!(
            info.height == height as u32,
            Error::Internal("png decode: height mismatch".to_string()),
        );
        ensure!(
            info.color_type == png::ColorType::Rgba,
            Error::Internal("png decode: color type mismatch".to_string()),
        );
        ensure!(
            info.bit_depth == png::BitDepth::Eight,
            Error::Internal("png decode: bit depth mismatch".to_string()),
        );

        let out_len = reader
            .output_buffer_size()
            .ok_or_else(|| Error::Internal("png decode: unknown output size".to_string()))
            .location(loc!())?;
        let mut decoded = vec![0u8; out_len];
        let frame = reader.next_frame(&mut decoded).location(loc!())?;
        decoded.truncate(frame.buffer_size());

        ensure!(
            decoded == rgba,
            Error::Internal("png decode: payload mismatch".to_string()),
        );
        Ok(())
    }

    #[test]
    fn png_rejects_length_mismatch() {
        let err = encode_png_rgba(&[0u8; 3], 1, 1).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("length mismatch"));
    }

    #[test]
    fn png_rejects_zero_dimensions() {
        let err = encode_png_rgba(&[], 0, 1).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("non-zero"));
    }
}
