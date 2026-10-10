use super::AppServerSession;
use color_eyre::eyre::Result;
use color_eyre::eyre::WrapErr;
use octopus_world_terminal_client::HostPath;
use std::path::Path;

impl AppServerSession {
    pub(crate) async fn fs_create_directory_all_path(&mut self, path: &HostPath) -> Result<()> {
        tokio::fs::create_dir_all(Path::new(path.as_str()))
            .await
            .wrap_err_with(|| format!("creating terminal host directory {path} failed"))
    }

    pub(crate) async fn fs_write_file_path(
        &mut self,
        path: &HostPath,
        bytes: Vec<u8>,
    ) -> Result<()> {
        tokio::fs::write(Path::new(path.as_str()), bytes)
            .await
            .wrap_err_with(|| format!("writing terminal host file {path} failed"))
    }

    pub(crate) async fn fs_read_file_path(&mut self, path: &HostPath) -> Result<Vec<u8>> {
        tokio::fs::read(Path::new(path.as_str()))
            .await
            .wrap_err_with(|| format!("reading terminal host file {path} failed"))
    }

    pub(crate) async fn fs_remove_path(&mut self, path: &HostPath) -> Result<()> {
        let target = Path::new(path.as_str());
        let metadata = tokio::fs::metadata(target)
            .await
            .wrap_err_with(|| format!("inspecting terminal host path {path} failed"))?;
        if metadata.is_dir() {
            tokio::fs::remove_dir_all(target)
                .await
                .wrap_err_with(|| format!("removing terminal host directory {path} failed"))
        } else {
            tokio::fs::remove_file(target)
                .await
                .wrap_err_with(|| format!("removing terminal host file {path} failed"))
        }
    }
}
