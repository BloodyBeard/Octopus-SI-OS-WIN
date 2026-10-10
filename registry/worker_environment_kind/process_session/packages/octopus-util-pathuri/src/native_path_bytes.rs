use crate::PathConvention;
use crate::PathUri;
use crate::PathUriParseError;

impl PathUri {
    pub fn join_native_bytes(&self, path: &[u8]) -> Result<Self, PathUriParseError> {
        if let Ok(path) = std::str::from_utf8(path) {
            return self.join(path);
        }
        if self.infer_path_convention() != Some(PathConvention::Posix)
            || self.opaque_fallback_bytes().is_some()
            || path.contains(&0)
        {
            return Err(PathUriParseError::InvalidFileUriPath {
                path: self.to_string(),
            });
        }
        let mut segments = if path.starts_with(b"/") {
            Vec::new()
        } else {
            self.encoded_path()
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        for component in path.split(|byte| *byte == b'/') {
            match component {
                b"" | b"." => {}
                b".." => {
                    segments.pop();
                }
                component => segments.push(urlencoding::encode_binary(component).into_owned()),
            }
        }
        let mut url = self.to_url();
        url.set_path(&format!("/{}", segments.join("/")));
        let uri = Self::try_from(url)?;
        if uri.infer_path_convention() != Some(PathConvention::Posix) {
            return Err(PathUriParseError::InvalidFileUriPath {
                path: uri.to_string(),
            });
        }
        Ok(uri)
    }
}
