mod json;
mod truncate;

pub use json::to_ascii_json_string;
pub use truncate::approx_bytes_for_tokens;
pub use truncate::approx_token_count;
pub use truncate::approx_tokens_from_byte_count;
pub use truncate::truncate_middle_chars;
pub use truncate::truncate_middle_with_token_budget;

pub fn sanitize_metric_tag_value(value: &str) -> String {
    const MAX_LEN: usize = 256;
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches('_');
    if trimmed.is_empty() || trimmed.chars().all(|ch| !ch.is_ascii_alphanumeric()) {
        return "unspecified".to_string();
    }
    if trimmed.len() <= MAX_LEN {
        trimmed.to_string()
    } else {
        trimmed[..MAX_LEN].to_string()
    }
}

#[allow(clippy::unwrap_used)]
pub fn find_uuids(s: &str) -> Vec<String> {
    static RE: std::sync::OnceLock<regex_lite::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex_lite::Regex::new(
            r"[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}",
        )
        .unwrap()
    });

    re.find_iter(s).map(|m| m.as_str().to_string()).collect()
}

#[cfg(test)]
#[allow(warnings, clippy::all)]
mod tests {
    use super::find_uuids;
    use super::sanitize_metric_tag_value;
    use pretty_assertions::assert_eq;

    #[test]
    fn find_uuids_finds_multiple() {
        let input =
            "x 00112233-4455-6677-8899-aabbccddeeff-k y 12345678-90ab-cdef-0123-456789abcdef";
        assert_eq!(
            find_uuids(input),
            vec![
                "00112233-4455-6677-8899-aabbccddeeff".to_string(),
                "12345678-90ab-cdef-0123-456789abcdef".to_string(),
            ]
        );
    }

    #[test]
    fn find_uuids_ignores_invalid() {
        let input = "not-a-uuid-1234-5678-9abc-def0-123456789abc";
        assert_eq!(find_uuids(input), Vec::<String>::new());
    }

    #[test]
    fn find_uuids_handles_non_ascii_without_overlap() {
        let input = "🙂 55e5d6f7-8a7f-4d2a-8d88-123456789012abc";
        assert_eq!(
            find_uuids(input),
            vec!["55e5d6f7-8a7f-4d2a-8d88-123456789012".to_string()]
        );
    }

    #[test]
    fn sanitize_metric_tag_value_trims_and_fills_unspecified() {
        let msg = "///";
        assert_eq!(sanitize_metric_tag_value(msg), "unspecified");
    }

    #[test]
    fn sanitize_metric_tag_value_replaces_invalid_chars() {
        let msg = "bad value!";
        assert_eq!(sanitize_metric_tag_value(msg), "bad_value");
    }
}
