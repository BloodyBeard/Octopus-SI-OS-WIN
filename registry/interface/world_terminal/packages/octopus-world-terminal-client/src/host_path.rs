use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPath(String);

impl HostPath {
    pub fn from_host(path: impl Into<String>) -> Self {
        Self(path.into())
    }

    pub fn from_absolute_str(raw: &str) -> Option<Self> {
        (raw.starts_with('/') || is_windows_absolute_path(raw)).then(|| Self(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn components(&self) -> Vec<&str> {
        let separators = if is_windows_absolute_path(&self.0) {
            &['/', '\\'][..]
        } else {
            &['/'][..]
        };
        self.0
            .split(separators)
            .filter(|part| !part.is_empty())
            .collect()
    }

    pub fn join(&self, segment: impl AsRef<str>) -> Self {
        let is_windows = is_windows_absolute_path(&self.0);
        let (path, separator) = if is_windows {
            (self.0.trim_end_matches(['/', '\\']), '\\')
        } else {
            (self.0.trim_end_matches('/'), '/')
        };
        Self(format!("{path}{separator}{}", segment.as_ref()))
    }
}

impl fmt::Display for HostPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || path.starts_with("\\\\")
        || path.starts_with("//")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_guard_and_components() {
        assert!(HostPath::from_absolute_str("relative/path").is_none());
        let path = HostPath::from_absolute_str("/octo/home/attachments/x").unwrap();
        assert_eq!(path.components(), vec!["octo", "home", "attachments", "x"]);
        assert_eq!(path.as_str(), "/octo/home/attachments/x");
    }

    #[test]
    fn join_uses_posix_separator() {
        let path = HostPath::from_host("/octo/home/");
        assert_eq!(path.join("attachments").as_str(), "/octo/home/attachments");
    }

    #[test]
    fn windows_absolute_join_and_split() {
        let path = HostPath::from_absolute_str("C:\\octo\\home").unwrap();
        assert_eq!(
            path.join("attachments").as_str(),
            "C:\\octo\\home\\attachments"
        );
        assert_eq!(path.components(), vec!["C:", "octo", "home"]);
    }
}
