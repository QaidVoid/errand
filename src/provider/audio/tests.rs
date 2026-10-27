//! Tests for the audio sniffer.

use super::audio_extension;

#[test]
fn an_id3_tag_and_a_frame_header_are_mp3() {
    assert_eq!(
        audio_extension(b"ID3\x04\x00\x00\x00\x00\x00\x00"),
        Some("mp3")
    );
    assert_eq!(audio_extension(&[0xFF, 0xFB, 0x90, 0x64]), Some("mp3"));
}

#[test]
fn headers_that_are_not_mpeg_audio_are_not_mp3() {
    // ADTS AAC.
    assert_eq!(audio_extension(&[0xFF, 0xF1, 0x50, 0x80]), None);
    // The reserved version `01`.
    assert_eq!(audio_extension(&[0xFF, 0xEB, 0x90, 0x64]), None);
    // The invalid bitrate index `1111`.
    assert_eq!(audio_extension(&[0xFF, 0xFB, 0xF0, 0x64]), None);
    // The reserved sample rate `11`.
    assert_eq!(audio_extension(&[0xFF, 0xFB, 0x9C, 0x64]), None);
}

#[test]
fn a_wave_file_is_wav_and_webp_is_not_audio() {
    assert_eq!(audio_extension(b"RIFF\0\0\0\0WAVEfmt "), Some("wav"));
    assert_eq!(audio_extension(b"RIFF\0\0\0\0WEBPVP8 "), None);
}

#[test]
fn text_and_empty_bytes_are_nothing() {
    assert_eq!(audio_extension(b"plain text"), None);
    assert_eq!(audio_extension(b""), None);
}
