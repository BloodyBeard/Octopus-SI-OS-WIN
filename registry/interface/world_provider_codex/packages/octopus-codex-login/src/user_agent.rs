pub fn user_agent() -> String {
    let raw = if let Some(program) = var_non_empty("TERM_PROGRAM") {
        match var_non_empty("TERM_PROGRAM_VERSION") {
            Some(version) => format!("{program}/{version}"),
            None => program,
        }
    } else if let Some(term) = var_non_empty("TERM") {
        term
    } else {
        "unknown".to_string()
    };

    sanitize_header_value(raw)
}

fn var_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn sanitize_header_value(value: String) -> String {
    value.replace(|c| !is_valid_header_value_char(c), "_")
}

fn is_valid_header_value_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '/'
}
