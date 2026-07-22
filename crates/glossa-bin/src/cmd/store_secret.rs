use std::io::{self, Read};

use anyhow::{anyhow, Context};

/// Reads a secret from stdin so it never appears in process arguments.
pub fn run(slot: &str) -> anyhow::Result<()> {
    let mut secret = String::new();
    io::stdin()
        .read_to_string(&mut secret)
        .context("failed to read secret from stdin")?;
    if secret.is_empty() {
        return Err(anyhow!("secret must not be empty"));
    }
    glossa_platform_linux::secret::store(slot, &secret)?;
    Ok(())
}
