use super::*;

pub(crate) fn safe_segment(value: &str, field: &str) -> Result<String, String> {
    let segment = value.trim();
    if segment.is_empty()
        || segment.len() > 96
        || !segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(format!(
            "{field} must be a non-empty segment of letters, digits, underscore, or hyphen"
        ));
    }
    Ok(segment.to_string())
}

pub(crate) fn safe_policy_axis_value(value: &str, field: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 192
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
    {
        return Err(format!(
            "{field} must contain only letters, digits, underscore, hyphen, or dot"
        ));
    }
    Ok(value.to_string())
}

pub(crate) fn require_absolute<'a>(field: &str, path: &'a Path) -> Result<&'a Path, String> {
    if !path.is_absolute() {
        return Err(format!("{field} must be an absolute path"));
    }
    Ok(path)
}
