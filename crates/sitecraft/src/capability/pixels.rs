//! The D4 evidence rules: the pixel rule (a render / canvas screenshot shows
//! something drawn) and the GLB header rule.
//!
//! Both are pure functions over bytes. Reading the bytes (and telling a
//! missing file from an unreadable one) is the caller's job.

use std::collections::BTreeMap;

/// A channel must differ from the background by more than this to count as
/// drawn (rulings D4).
pub const CHANNEL_THRESHOLD: u8 = 16;
/// Minimum drawn share, in percent (rulings D4).
pub const MIN_DRAWN_PERCENT: u64 = 1;

/// Outcome of the pixel rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pixels {
    /// At least 1 % of the pixels are drawn.
    Drawn {
        /// Drawn pixel count.
        drawn: u64,
        /// Total pixel count.
        total: u64,
    },
    /// Violation; carries why (not a PNG, zero-sized, or under 1 % drawn).
    NotDrawn(String),
}

/// Apply the D4 pixel rule to PNG `bytes`.
///
/// Background = the most frequent RGB of the 4 corner pixels (alpha
/// ignored); when that is not unique, every tied candidate is tried and the
/// one giving the **smallest** drawn ratio wins (the restricted side). A
/// pixel is drawn when some R/G/B channel differs from the background by more
/// than [`CHANNEL_THRESHOLD`]. Drawn ratio >= 1 % → [`Pixels::Drawn`];
/// below 1 %, width or height 0, or bytes that do not decode as a PNG →
/// [`Pixels::NotDrawn`].
#[must_use]
pub fn judge_png(bytes: &[u8]) -> Pixels {
    let (w, h, rgb) = match decode_rgb(bytes) {
        Ok(x) => x,
        Err(e) => return Pixels::NotDrawn(e),
    };
    if w == 0 || h == 0 {
        return Pixels::NotDrawn(format!("the image is {w}x{h} (empty)"));
    }
    let at = |x: usize, y: usize| rgb[y * w + x];
    let corners = [at(0, 0), at(w - 1, 0), at(0, h - 1), at(w - 1, h - 1)];
    let mut freq: BTreeMap<[u8; 3], usize> = BTreeMap::new();
    for c in corners {
        *freq.entry(c).or_insert(0) += 1;
    }
    let best = freq.values().copied().max().unwrap_or(0);
    let candidates: Vec<[u8; 3]> = freq
        .iter()
        .filter(|(_, n)| **n == best)
        .map(|(c, _)| *c)
        .collect();
    let total = rgb.len() as u64;
    let mut drawn_min: Option<(u64, [u8; 3])> = None;
    for bg in candidates {
        let drawn = rgb.iter().filter(|p| is_drawn(**p, bg)).count() as u64;
        if drawn_min.is_none_or(|(d, _)| drawn < d) {
            drawn_min = Some((drawn, bg));
        }
    }
    let Some((drawn, bg)) = drawn_min else {
        return Pixels::NotDrawn("the image has no corner pixels".to_string());
    };
    if drawn.saturating_mul(100) >= total.saturating_mul(MIN_DRAWN_PERCENT) {
        Pixels::Drawn { drawn, total }
    } else {
        Pixels::NotDrawn(format!(
            "only {drawn} of {total} pixels ({w}x{h}) differ from the background rgb({},{},{}) by more than {CHANNEL_THRESHOLD} on a channel; at least {MIN_DRAWN_PERCENT}% must be drawn",
            bg[0], bg[1], bg[2]
        ))
    }
}

fn is_drawn(p: [u8; 3], bg: [u8; 3]) -> bool {
    p.iter()
        .zip(bg.iter())
        .any(|(a, b)| a.abs_diff(*b) > CHANNEL_THRESHOLD)
}

/// Decode to (width, height, RGB pixels row-major).
fn decode_rgb(bytes: &[u8]) -> Result<(usize, usize, Vec<[u8; 3]>), String> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec
        .read_info()
        .map_err(|e| format!("not readable as a PNG: {e}"))?;
    let (w, h) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if w == 0 || h == 0 {
        return Ok((w as usize, h as usize, Vec::new()));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| format!("the PNG ({w}x{h}) is too large to decode"))?;
    let mut buf = vec![0u8; size];
    let out = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("not readable as a PNG: {e}"))?;
    let channels = match out.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => {
            return Err("the PNG palette was not expanded (unsupported encoding)".to_string())
        }
    };
    let (w, h) = (out.width as usize, out.height as usize);
    let mut px = Vec::with_capacity(w * h);
    for y in 0..h {
        let row = &buf[y * out.line_size..y * out.line_size + w * channels];
        for x in 0..w {
            let p = &row[x * channels..x * channels + channels];
            px.push(match channels {
                1 | 2 => [p[0], p[0], p[0]],
                _ => [p[0], p[1], p[2]],
            });
        }
    }
    Ok((w, h, px))
}

/// The D4 GLB rule: the first 4 bytes are `glTF`, the next u32 LE (version)
/// is 2, and the u32 LE after it (length) equals the byte count. `Err`
/// (violation) carries which part failed; 0 bytes fails.
pub fn judge_glb(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 12 {
        return Err(format!(
            "the GLB is {} bytes, shorter than the 12-byte header",
            bytes.len()
        ));
    }
    if &bytes[0..4] != b"glTF" {
        return Err(format!(
            "the GLB does not start with `glTF` (got {:?})",
            String::from_utf8_lossy(&bytes[0..4])
        ));
    }
    let u32le = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    let version = u32le(4);
    if version != 2 {
        return Err(format!("the GLB header version is {version}, not 2"));
    }
    let length = u32le(8);
    if u64::from(length) != bytes.len() as u64 {
        return Err(format!(
            "the GLB header length is {length} but the file has {} bytes",
            bytes.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&f(x, y));
            }
        }
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(&data).unwrap();
        }
        out
    }

    #[test]
    fn one_percent_boundary() {
        let img = |n: u32| {
            png(100, 100, move |x, y| {
                if y == 50 && x < n {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                }
            })
        };
        assert!(matches!(
            judge_png(&img(100)),
            Pixels::Drawn { drawn: 100, .. }
        ));
        assert!(matches!(judge_png(&img(99)), Pixels::NotDrawn(_)));
    }

    #[test]
    fn glb_header() {
        let mut g = b"glTF".to_vec();
        g.extend_from_slice(&2u32.to_le_bytes());
        g.extend_from_slice(&12u32.to_le_bytes());
        assert!(judge_glb(&g).is_ok());
        assert!(judge_glb(&[]).is_err());
        let mut v1 = g.clone();
        v1[4] = 1;
        assert!(judge_glb(&v1).is_err());
    }
}
