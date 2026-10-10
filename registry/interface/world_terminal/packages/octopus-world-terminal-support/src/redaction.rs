use std::sync::OnceLock;

use regex_lite::Captures;
use regex_lite::Regex;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactedText {
    pub text: String,
    pub redaction_count: usize,
}

pub fn redact_support_text(input: &str, private_root: Option<&str>) -> RedactedText {
    let mut text = input.to_string();
    let mut redaction_count = 0usize;

    if let Some(root) = private_root.filter(|root| !root.is_empty()) {
        let count = text.matches(root).count();
        if count > 0 {
            text = text.replace(root, "$OCTOPUS_HOME");
            redaction_count += count;
        }
    }

    let (next, count) = replace_matches(bearer_regex(), &text, |_captures| {
        "[REDACTED_AUTH]".to_string()
    });
    text = next;
    redaction_count += count;

    let (next, count) = replace_secret_assignments(&text);
    text = next;
    redaction_count += count;

    let (next, count) = replace_matches(token_regex(), &text, |_captures| {
        "[REDACTED_TOKEN]".to_string()
    });
    text = next;
    redaction_count += count;

    let (next, count) = replace_matches(url_regex(), &text, |captures| {
        redact_url(captures.get(0).map_or("", |value| value.as_str()))
    });
    text = next;
    redaction_count += count;

    RedactedText {
        text,
        redaction_count,
    }
}

fn replace_secret_assignments(input: &str) -> (String, usize) {
    replace_matches(secret_assignment_regex(), input, |captures| {
        format!("{}{}[REDACTED]", &captures[1], &captures[2])
    })
}

fn replace_matches(
    regex: &Regex,
    input: &str,
    replacement: impl Fn(&Captures<'_>) -> String,
) -> (String, usize) {
    let mut count = 0usize;
    let output = regex
        .replace_all(input, |captures: &Captures<'_>| {
            count += 1;
            replacement(captures)
        })
        .into_owned();
    (output, count)
}

fn redact_url(raw: &str) -> String {
    let split = raw
        .find(|character: char| matches!(character, ')' | ']' | '}' | ',' | ';'))
        .unwrap_or(raw.len());
    let (candidate, suffix) = raw.split_at(split);
    let Ok(mut url) = Url::parse(candidate) else {
        return format!("[REDACTED_URL]{suffix}");
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    if url.query().is_some() {
        url.set_query(Some("REDACTED"));
    }
    url.set_fragment(None);
    format!("{url}{suffix}")
}

fn secret_assignment_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(
            r"(?i)(authorization|proxy-authorization|cookie|set-cookie|api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)([ \t]*[:=][ \t]*)([^ \t\r\n,;]+)",
        )
        .expect("support secret regex is valid")
    })
}

fn bearer_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer|basic)[ \t]+[^ \t\r\n,;]+")
            .expect("support authorization regex is valid")
    })
}

fn token_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| {
        Regex::new(r"(?i)\b(sk-[a-z0-9_-]{10,}|gh[pousr]_[a-z0-9]{10,}|github_pat_[a-z0-9_]{10,})")
            .expect("support token regex is valid")
    })
}

fn url_regex() -> &'static Regex {
    static VALUE: OnceLock<Regex> = OnceLock::new();
    VALUE.get_or_init(|| Regex::new(r#"https?://[^\s<>'\"]+"#).expect("support URL regex is valid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_credentials_tokens_queries_and_private_root() {
        let source = "Authorization: Bearer abcdef token=secret-value https://user:pass@example.test/a?q=secret#fragment /private/home/log sk-abcdefghijklmnop";
        let result = redact_support_text(source, Some("/private/home"));
        assert!(!result.text.contains("abcdef"));
        assert!(!result.text.contains("secret-value"));
        assert!(!result.text.contains("user:pass"));
        assert!(!result.text.contains("q=secret"));
        assert!(!result.text.contains("abcdefghijklmnop"));
        assert!(result.text.contains("$OCTOPUS_HOME/log"));
        assert!(result.redaction_count >= 5);
    }
}
