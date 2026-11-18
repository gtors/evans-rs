use std::path::Path;

use anyhow::{Context, Result};
use prost_reflect::DescriptorPool;

use super::Idl;

impl Idl {
    pub fn from_files(proto_path: &[String], proto_file: &[String]) -> Result<Self> {
        let files: Vec<&Path> = proto_file.iter().map(Path::new).collect();
        let includes: Vec<&Path> = if proto_path.is_empty() {
            vec![Path::new(".")]
        } else {
            proto_path.iter().map(Path::new).collect()
        };

        let fds = protox::compile(files, includes).context("failed to compile proto files")?;

        let pool = DescriptorPool::from_file_descriptor_set(fds)
            .map_err(|e| anyhow::anyhow!("failed to build descriptor pool: {e}"))?;

        Ok(Self::from_pool(pool))
    }
}
