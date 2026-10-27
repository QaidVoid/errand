//! What audio bytes declare themselves to be, read from the bytes.
//!
//! Beside [`crate::provider::images`], which is about drawing, so the
//! sniffing rule sits here rather than in a module documented as image-only.

/// The file extension of the audio `bytes` declare themselves: MP3, by an
/// `ID3` tag or an MPEG audio frame header, or WAV.
///
/// The type is read from the bytes rather than from a declared `mimeType`,
/// which is the agent's word.
pub fn audio_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"ID3") || is_mpeg_frame(bytes) {
        Some("mp3")
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE" {
        Some("wav")
    } else {
        None
    }
}

/// Whether the bytes begin with an MPEG audio frame header. It needs three
/// bytes: the sync, a version and layer byte, and a bitrate and sample-rate
/// index byte. The reserved version `01` and layer `00`, the invalid bitrate
/// index `1111` and the reserved sample rate `11` are kept out, which leaves
/// ADTS AAC (`FF F1`) behind.
fn is_mpeg_frame(bytes: &[u8]) -> bool {
    if bytes.len() < 3 {
        return false;
    }
    let (b0, b1, b2) = (bytes[0], bytes[1], bytes[2]);
    b0 == 0xFF
        && b1 & 0xE0 == 0xE0
        && (b1 >> 3) & 0b11 != 0b01
        && (b1 >> 1) & 0b11 != 0b00
        && b2 >> 4 != 0b1111
        && (b2 >> 2) & 0b11 != 0b11
}

#[cfg(test)]
mod tests;
