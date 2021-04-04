//! This is incomplete, please look at `rocks.rs` instead

use super::ImageCache;
use std::{io, path};
use tokio::fs;

pub struct FileSystemCache {
    base: path::PathBuf
}

impl FileSystemCache {
    /// Find the full path including the base directory of an image on the disk.
    ///
    /// **WARNING**: This does not verify that the image exists on the disk, it only specifies where
    /// it should exist.
    fn get_path(&self, chap_hash: &str, image: &str, saver: bool) -> path::PathBuf {
        // find the chapter segment of the path
        //
        // Example: if chapter was "8172a46adc798f4f4ace6663322a383e" then the path would end up
        // being "81/72/8172a46adc798f4f4ace6663322a383e/IMG.png"
        let chap_path = format!("{}/{}/{}", &chap_hash[0..2], &chap_hash[2..4], chap_hash);

        // clone the base path then push relative path to image
        let mut full_path = self.base.clone();
        full_path.push(if saver {
            format!("{}/saver-{}", chap_path, image)
        } else {
            format!("{}/{}", chap_path, image)
        });
        full_path
    }

    /// Create a new [`FileSystemCache`](Self) instance, automatically creating the path if it
    /// doesn't exist in the meantime.
    ///
    /// This will fail if the base path provided is a file instead of a directory, or if there is
    /// some other io Error (like an error reading metadata because of permission levels)
    pub async fn new(base: impl AsRef<path::Path>) -> io::Result<Self> {
        // find the metadata of the path, creating if it doesn't exist already
        let meta = match fs::metadata(&base).await {
            // metadata found, so immediately return that
            Ok(m) => m,

            Err(e) => match e.kind() {
                // if folder not found, create it and refind metadata
                io::ErrorKind::NotFound => {
                    fs::create_dir_all(&base).await?;
                    fs::metadata(&base).await?
                }

                // other errors are unexpected and can just pass up the stack
                _ => return Err(e)
            }
        };

        // if it's not a directory then return an error
        // this error is the same as if you were to open try to open a file but it was actually a
        // directory (at least on windows)
        if !meta.is_dir() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }

        Ok(Self {
            base: base.as_ref().into()
        })
    }

    pub async fn save_on_disk(&self, chap_hash: &str, image: &str, buf: &[u8]) -> io::Result<()> {
        // TODO: don't hardcode saver
        let path = self.get_path(chap_hash, image, false);

        // create directory up to file if it doesn't exist
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).await?;
        }

        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)
            .await?;

        use tokio::io::AsyncWriteExt;
        f.write_all(buf).await?;

        Ok(())
    }
}
