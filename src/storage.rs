use std::{
    fs::ReadDir,
    io::Read,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{blobstorage::BlobStorage, lockmap::LockMap};

pub trait Storage {
    async fn get(&self, path: &str) -> std::io::Result<(FileMetadata, Vec<u8>)>;
    async fn head(&self, path: &str) -> std::io::Result<FileMetadata>;
    async fn put(
        &self,
        path: &str,
        version: DateTime<Utc>,
        content: &[u8],
        content_is_gzipped: bool,
        checksum: Option<[u8; 32]>,
        logical_size: Option<usize>,
    ) -> std::io::Result<()>;
    async fn delete(&self, path: &str, max_version: DateTime<Utc>) -> std::io::Result<()>;
    async fn list(
        &self,
        path: &str,
        max_version: DateTime<Utc>,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<(String, FileMetadata)>>>;
}

pub struct LocalStorage {
    locks: LockMap<String>,
    blobs: BlobStorage,
    metadata: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Compression {
    None,
    Gzip,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FileMetadata {
    pub version: DateTime<Utc>,
    pub checksum: [u8; 32],
    pub compression: Compression,
    pub decompressed_size: usize,
}

impl FileMetadata {
    fn read(path: &Path) -> std::io::Result<Self> {
        serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

struct FileLister {
    readdir_stack: Vec<ReadDir>,
    metadata: PathBuf,
    max_version: DateTime<Utc>,
}

impl Iterator for FileLister {
    type Item = std::io::Result<(String, FileMetadata)>;

    fn next(&mut self) -> Option<Self::Item> {
        macro_rules! try_ {
            ($value: expr) => {
                match $value {
                    Ok(ok) => ok,
                    Err(e) => return Some(Err(e)),
                }
            };
        }

        loop {
            let current = self.readdir_stack.last_mut()?;
            match current.next() {
                Some(Err(e)) => return Some(Err(e)),
                Some(Ok(e)) => match e.file_type() {
                    Ok(ft) if ft.is_dir() => self.readdir_stack.push(try_!(e.path().read_dir())),
                    Ok(ft) if ft.is_file() => {
                        let path = e.path();
                        let metadata = try_!(FileMetadata::read(&path));
                        if metadata.version <= self.max_version {
                            let relative = path.strip_prefix(&self.metadata).unwrap();
                            return Some(Ok((relative.to_str().unwrap().to_string(), metadata)));
                        }
                    }
                    Ok(_) => (),
                    Err(e) => return Some(Err(e)),
                },
                None => {
                    self.readdir_stack.pop().unwrap();
                }
            }
        }
    }
}

impl LocalStorage {
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok({
            let result = Self {
                locks: LockMap::new(),
                blobs: BlobStorage::create(root.join("blobs"))?,
                metadata: root.join("metadata"),
            };
            std::fs::create_dir_all(&result.metadata)?;
            result
        })
    }

    fn read_meta_for(&self, path: &str) -> std::io::Result<FileMetadata> {
        FileMetadata::read(&self.metadata.join(path))
    }
}

impl Storage for LocalStorage {
    async fn get(&self, path: &str) -> std::io::Result<(FileMetadata, Vec<u8>)> {
        let _guard = self.locks.lock_ref(path).await;
        let metadata = self.read_meta_for(path)?;
        let content = self.blobs.read(&metadata.checksum)?;
        Ok((metadata, content))
    }

    async fn head(&self, path: &str) -> std::io::Result<FileMetadata> {
        let _guard = self.locks.lock_ref(path).await;
        let metadata = self.read_meta_for(path)?;
        Ok(metadata)
    }

    async fn put(
        &self,
        path: &str,
        version: DateTime<Utc>,
        content: &[u8],
        content_is_gzipped: bool,
        checksum: Option<[u8; 32]>,
        logical_size: Option<usize>,
    ) -> std::io::Result<()> {
        let (decompressed_size, checksum, mut compressed) = if !content_is_gzipped {
            (
                content.len(),
                checksum.unwrap_or_else(|| Sha256::new().chain_update(content).finalize().into()),
                Box::new(flate2::read::GzEncoder::new(
                    std::io::Cursor::new(content),
                    flate2::Compression::new(9),
                )) as Box<dyn Read + Send>,
            )
        } else if let (Some(checksum), Some(logical_size)) = (checksum, logical_size) {
            (
                logical_size,
                checksum,
                Box::new(std::io::Cursor::new(content)) as Box<dyn Read + Send>,
            )
        } else {
            let mut decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(content));
            let mut buf = [0; 4096];
            let mut decompressed_size = 0;
            let mut checksum = Sha256::new();

            loop {
                let nread = decoder.read(&mut buf)?;
                if nread == 0 {
                    break;
                }
                Digest::update(&mut checksum, &buf[..nread]);
                decompressed_size += nread;
            }

            (
                decompressed_size,
                checksum.finalize().into(),
                Box::new(std::io::Cursor::new(content)) as Box<dyn Read + Send>,
            )
        };

        let _guard = self.locks.lock_ref(path);
        match self.read_meta_for(path) {
            Ok(meta) => {
                if meta.version > version {
                    return Ok(());
                }
                self.blobs.decref(&meta.checksum).await?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }

        let dest_meta = self.metadata.join(path);
        std::fs::create_dir_all(dest_meta.parent().unwrap())?;

        self.blobs.write(&checksum, &mut compressed).await?;

        std::fs::write(
            dest_meta,
            serde_json::to_string(&FileMetadata {
                version,
                checksum,
                compression: Compression::Gzip,
                decompressed_size,
            })
            .unwrap(),
        )?;

        Ok(())
    }

    async fn delete(&self, path: &str, max_version: DateTime<Utc>) -> std::io::Result<()> {
        let _guard = self.locks.lock_ref(path).await;
        let metadata = self.read_meta_for(path)?;
        if metadata.version <= max_version {
            self.blobs.decref(&metadata.checksum).await?;
            std::fs::remove_file(self.metadata.join(path))?;
        }
        Ok(())
    }

    async fn list(
        &self,
        path: &str,
        max_version: DateTime<Utc>,
    ) -> std::io::Result<impl Iterator<Item = std::io::Result<(String, FileMetadata)>>> {
        let metadata = self.metadata.join(path);
        let iter = metadata.read_dir()?;
        Ok(FileLister {
            metadata,
            max_version,
            readdir_stack: vec![iter],
        })
    }
}
