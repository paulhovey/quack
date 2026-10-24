//! Images in user input and tool results: base64 payloads with their media type and,
//! when the header says so, their pixel size. Adapters translate these to each wire
//! format; the bytes are probed and encoded here so every caller agrees.

use serde::{Deserialize, Serialize};

/// The largest image sent to a provider, raw bytes. Anthropic rejects images over 5 MB
/// base64 (about 3.7 MB raw); 3 MB leaves headroom everywhere.
pub const MAX_RAW_BYTES: usize = 3 * 1024 * 1024;

/// One image as it travels through the history: base64 (standard, padded), its media
/// type, and its pixel size when the file's header reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Image {
    /// `image/png`, `image/jpeg`, `image/gif` or `image/webp`.
    pub media_type: String,
    /// The whole file, base64-encoded.
    pub data: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl Image {
    /// Probe and encode `bytes`, refusing anything a provider would reject.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_RAW_BYTES {
            return Err(format!(
                "the image is {} KB; the limit is {} KB, so downscale it first",
                bytes.len() / 1024,
                MAX_RAW_BYTES / 1024
            ));
        }
        let (media_type, width, height) =
            probe(bytes).ok_or_else(|| "not a png, jpeg, gif or webp image".to_owned())?;
        Ok(Self {
            media_type: media_type.to_owned(),
            data: base64(bytes),
            width,
            height,
        })
    }

    /// Read, probe and encode a file.
    pub fn from_path(path: &std::path::Path) -> Result<Self, String> {
        let shown = path.display();
        let bytes = std::fs::read(path).map_err(|e| format!("{shown}: {e}"))?;
        Self::from_bytes(&bytes).map_err(|e| format!("{shown}: {e}"))
    }

    /// A data URL, the form OpenAI-compatible servers take.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }

    /// Rough token count. Both Anthropic (w*h/750) and OpenAI (tile-based) land in this
    /// area for the sizes agents send; a flat floor keeps small icons from being free.
    pub fn tokens(&self) -> u32 {
        match (self.width, self.height) {
            (Some(w), Some(h)) => u32::try_from((u64::from(w) * u64::from(h)) / 750)
                .unwrap_or(u32::MAX)
                .max(256),
            _ => 1_000,
        }
    }
}

/// The image's media type and pixel size, or `None` when the bytes are not an image we
/// send. Dimensions come from the headers, so nothing is decoded.
pub fn probe(bytes: &[u8]) -> Option<(&'static str, Option<u32>, Option<u32>)> {
    match bytes.first()? {
        0x89 => png(bytes),
        b'G' => gif(bytes),
        0xFF => jpeg(bytes),
        b'R' => webp(bytes),
        _ => None,
    }
}

fn png(b: &[u8]) -> Option<(&'static str, Option<u32>, Option<u32>)> {
    b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A])
        .then(|| {
            let w = be32(b.get(16..20)?);
            let h = be32(b.get(20..24)?);
            Some(("image/png", Some(w), Some(h)))
        })
        .flatten()
}

fn gif(b: &[u8]) -> Option<(&'static str, Option<u32>, Option<u32>)> {
    (b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"))
        .then(|| {
            let w = u32::from(u16::from_le_bytes([*b.get(6)?, *b.get(7)?]));
            let h = u32::from(u16::from_le_bytes([*b.get(8)?, *b.get(9)?]));
            Some(("image/gif", Some(w), Some(h)))
        })
        .flatten()
}

fn jpeg(b: &[u8]) -> Option<(&'static str, Option<u32>, Option<u32>)> {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    while let Some(marker) = b.get(at).copied() {
        if marker != 0xFF {
            return Some(("image/jpeg", None, None));
        }
        let kind = *b.get(at + 1)?;
        // SOF0..SOF15 except DHT, JPG and DAC carry the frame size.
        if (0xC0..=0xCF).contains(&kind) && kind != 0xC4 && kind != 0xC8 && kind != 0xCC {
            let h = u32::from(u16::from_be_bytes([*b.get(at + 5)?, *b.get(at + 6)?]));
            let w = u32::from(u16::from_be_bytes([*b.get(at + 7)?, *b.get(at + 8)?]));
            return Some(("image/jpeg", Some(w), Some(h)));
        }
        let len = usize::from(u16::from_be_bytes([*b.get(at + 2)?, *b.get(at + 3)?]));
        at += 2 + len;
    }
    Some(("image/jpeg", None, None))
}

fn webp(b: &[u8]) -> Option<(&'static str, Option<u32>, Option<u32>)> {
    if !(b.starts_with(b"RIFF") && b.get(8..12)? == b"WEBP") {
        return None;
    }
    let chunk = b.get(12..16)?;
    match chunk {
        // VP8X: the canvas size, minus one, three bytes little-endian each.
        b"VP8X" => {
            let w =
                u32::from(*b.get(24)?) | u32::from(*b.get(25)?) << 8 | u32::from(*b.get(26)?) << 16;
            let h =
                u32::from(*b.get(27)?) | u32::from(*b.get(28)?) << 8 | u32::from(*b.get(29)?) << 16;
            Some(("image/webp", Some(w + 1), Some(h + 1)))
        }
        // A lossy keyframe: the size is a 14-bit field after the start code.
        b"VP8 " => {
            if b.get(23..26)? != [0x9D, 0x01, 0x2A] {
                return Some(("image/webp", None, None));
            }
            let w = u16::from_le_bytes([*b.get(26)?, *b.get(27)?]) & 0x3FFF;
            let h = u16::from_le_bytes([*b.get(28)?, *b.get(29)?]) & 0x3FFF;
            Some(("image/webp", Some(u32::from(w)), Some(u32::from(h))))
        }
        // Lossless: width and height, minus one, packed as 14-bit little-endian bits.
        b"VP8L" => {
            if b.get(20)? != &0x2F {
                return Some(("image/webp", None, None));
            }
            let bits = u32::from(*b.get(21)?)
                | u32::from(*b.get(22)?) << 8
                | u32::from(*b.get(23)?) << 16
                | u32::from(*b.get(24)?) << 24;
            let w = (bits & 0x3FFF) + 1;
            let h = ((bits >> 14) & 0x3FFF) + 1;
            Some(("image/webp", Some(w), Some(h)))
        }
        _ => Some(("image/webp", None, None)),
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from(b[0]) << 24 | u32::from(b[1]) << 16 | u32::from(b[2]) << 8 | u32::from(b[3])
}

/// Standard base64 with padding, enough for data URLs and the `images` fields.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 0x3F] as char);
        out.push(ALPHABET[(n >> 12) as usize & 0x3F] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 0x3F] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 0x3F] as char
        } else {
            '='
        });
    }
    out
}

/// Whether a file starts like an image we send. Reads the header, so it is cheap to
/// call on pasted paths before anything is encoded.
pub fn is_image(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut header = [0; 32];
    let Ok(n) = std::fs::File::open(path).and_then(|mut f| f.read(&mut header)) else {
        return false;
    };
    probe(&header[..n]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x1 red/blue PNG.
    const PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, b'I', b'H', b'D', b'R', 0,
        0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0, 0x7D, 0x0C, 0xC0, 0xFA,
    ];

    #[test]
    fn probes_png_gif_jpeg() {
        assert_eq!(probe(PNG), Some(("image/png", Some(2), Some(1))));
        assert_eq!(
            probe(b"GIF89a\x05\x00\x03\x00"),
            Some(("image/gif", Some(5), Some(3)))
        );
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xC0, 0, 11, 8, 0, 4, 0, 6, 3, 0, 0, 0, 0xFF, 0xD9,
        ];
        assert_eq!(probe(&jpeg), Some(("image/jpeg", Some(6), Some(4))));
        assert_eq!(probe(b"hello"), None);
    }

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn builds_and_limits_images() {
        let img = Image::from_bytes(PNG).expect("png");
        assert_eq!(img.media_type, "image/png");
        assert_eq!(img.width, Some(2));
        assert!(img.data_url().starts_with("data:image/png;base64,"));
        assert_eq!(img.tokens(), 256);
        let big = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        let mut big = big;
        big.resize(MAX_RAW_BYTES + 1, 0);
        let err = Image::from_bytes(&big).expect_err("too large");
        assert!(err.contains("limit"), "{err}");
    }
}
