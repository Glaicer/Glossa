use anyhow::{anyhow, Context};

use crate::bootstrap::load_config;

pub async fn run(config_path: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let config_path =
        config_path.ok_or_else(|| anyhow!("`glossa migrate-secrets` requires --config <path>"))?;
    let mut config = load_config(&config_path).await?;
    glossa_platform_linux::secret::migrate_plaintext_api_keys(&config_path, &mut config)
        .context("failed to migrate API keys")?;
    Ok(())
}
