use http::HeaderMap;

pub trait ExecServerAuthHeaders: Send + Sync {
    fn add_auth_headers(&self, headers: &mut HeaderMap);

    fn to_auth_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        self.add_auth_headers(&mut headers);
        headers
    }
}

pub type SharedExecServerAuth = std::sync::Arc<dyn ExecServerAuthHeaders>;

pub fn ensure_rustls_crypto_provider() {}

pub fn build_reqwest_client_with_custom_ca(
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, reqwest::Error> {
    builder.build()
}

pub mod shell_detect {
    use std::path::PathBuf;

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct DetectedShell {
        pub shell_path: PathBuf,
    }

    impl DetectedShell {
        pub fn name(&self) -> String {
            self.shell_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("sh")
                .to_string()
        }
    }

    pub fn default_user_shell() -> DetectedShell {
        let shell_path = std::env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| PathBuf::from("/bin/sh"));
        DetectedShell { shell_path }
    }
}
