use color_eyre::eyre::Result;

pub(crate) fn message(surface: &str) -> String {
    format!(
        "{surface}: the active terminal interfaces declare no artifact or file-tool path for this action, so it fails closed"
    )
}

pub(crate) fn fail<T>(surface: &str) -> Result<T> {
    color_eyre::eyre::bail!("{}", message(surface))
}
