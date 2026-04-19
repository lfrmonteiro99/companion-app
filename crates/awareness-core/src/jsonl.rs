use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;

pub struct JsonlWriter {
    path: PathBuf,
}

impl JsonlWriter {
    pub async fn new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Ok(Self { path })
    }

    pub async fn append<T: Serialize>(&self, value: &T) -> Result<()> {
        let mut line = serde_json::to_string(value)?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;

        file.write_all(line.as_bytes()).await?;
        file.flush().await?;
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}
