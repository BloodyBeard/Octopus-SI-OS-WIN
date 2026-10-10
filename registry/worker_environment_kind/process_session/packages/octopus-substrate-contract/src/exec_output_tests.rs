use super::StreamOutput;
use pretty_assertions::assert_eq;

#[test]
fn test_utf8_shell_output() {
    assert_eq!(decode_shell_output("пример".as_bytes()), "пример");
}

#[test]
fn test_cp1251_shell_output() {
    assert_eq!(decode_shell_output(b"\xEF\xF0\xE8\xEC\xE5\xF0"), "пример");
}

#[test]
fn test_cp866_shell_output() {
    assert_eq!(decode_shell_output(b"\xAF\xE0\xA8\xAC\xA5\xE0"), "пример");
}

#[test]
fn test_windows_1252_smart_decoding() {
    assert_eq!(
        decode_shell_output(b"\x93\x94 test \x96 dash"),
        "\u{201C}\u{201D} test \u{2013} dash"
    );
}

#[test]
fn test_smart_decoding_improves_over_lossy_utf8() {
    let bytes = b"\x93\x94 test \x96 dash";
    assert!(
        String::from_utf8_lossy(bytes).contains('\u{FFFD}'),
        "lossy UTF-8 should inject replacement chars"
    );
    assert_eq!(
        decode_shell_output(bytes),
        "\u{201C}\u{201D} test \u{2013} dash",
        "smart decoding should keep curly quotes intact"
    );
}

#[test]
fn test_mixed_ascii_and_legacy_encoding() {
    assert_eq!(decode_shell_output(b"Output: caf\xE9"), "Output: café");
}

#[test]
fn test_pure_latin1_shell_output() {
    assert_eq!(decode_shell_output(b"caf\xE9"), "café");
}

#[test]
fn test_invalid_bytes_still_fall_back_to_lossy() {
    let bytes = b"\xFF\xFE\xFD";
    assert_eq!(decode_shell_output(bytes), String::from_utf8_lossy(bytes));
}

fn decode_shell_output(bytes: &[u8]) -> String {
    StreamOutput {
        text: bytes.to_vec(),
        truncated_after_lines: None,
    }
    .from_utf8_lossy()
    .text
}
