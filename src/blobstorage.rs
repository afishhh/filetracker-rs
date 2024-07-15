use std::{
    fs::Metadata,
    io::Read,
    path::{Path, PathBuf},
};

use crate::{lockmap::LockMap, util::bytes_to_hex};

fn read_usize(path: &Path) -> std::io::Result<usize> {
    std::fs::read_to_string(path)?
        .parse::<usize>()
        .map_err(|x| std::io::Error::new(std::io::ErrorKind::InvalidData, x))
}

pub struct BlobStorage {
    locks: LockMap<[u8; 32]>,
    blobs: PathBuf,
}

impl BlobStorage {
    pub fn create(directory: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&directory)?;
        Ok(Self {
            locks: LockMap::new(),
            blobs: directory,
        })
    }

    fn path_to_blob(&self, sha256: &[u8; 32]) -> PathBuf {
        let hex = bytes_to_hex(sha256);

        self.blobs.join(&hex[0..2]).join(&hex[2..])
    }

    pub async fn write(&self, sha256: &[u8; 32], data: &mut impl Read) -> std::io::Result<bool> {
        let _guard = self.locks.lock_ref(sha256).await;
        let path = self.path_to_blob(sha256);
        let count_path = path.with_extension("count");
        if !path.exists() {
            let tmp_path = path.with_extension("tmp");
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::io::copy(data, &mut std::fs::File::create(&tmp_path)?)?;
            std::fs::rename(tmp_path, path)?;
            std::fs::write(count_path, b"1").map(|_| true)
        } else {
            std::fs::write(
                &count_path,
                (read_usize(&count_path)? + 1).to_string(),
            )
            .map(|_| false)
        }
    }

    pub fn read(&self, sha256: &[u8; 32]) -> std::io::Result<Vec<u8>> {
        std::fs::read(self.path_to_blob(sha256))
    }

    pub fn metadata(&self, sha256: &[u8; 32]) -> std::io::Result<Metadata> {
        self.path_to_blob(sha256).metadata()
    }

    pub async fn decref(&self, sha256: &[u8; 32]) -> std::io::Result<()> {
        let _guard = self.locks.lock_ref(sha256).await;
        let path = self.path_to_blob(sha256);
        let count_path = path.with_extension("count");
        let refs = read_usize(&count_path)?;

        if refs == 1 {
            std::fs::remove_file(count_path)?;
            std::fs::remove_file(path)
        } else {
            std::fs::write(count_path, (refs - 1).to_string())
        }
    }
}
