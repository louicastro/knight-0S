#![cfg_attr(not(test), no_std)]
//! KnightOS Virtual Filesystem (`knightos-vfs`) routing paths to mounted filesystems.

pub use knightfs::{BlockDevice, FileStat, FsError, KnightFs};

/// Single-root Virtual Filesystem backed by a mounted [`KnightFs`].
pub struct Vfs<D: BlockDevice> {
    root: KnightFs<D>,
}

impl<D: BlockDevice> Vfs<D> {
    pub fn format_and_mount(dev: D) -> Result<Self, FsError> {
        Ok(Self {
            root: KnightFs::format(dev)?,
        })
    }

    pub fn mount(dev: D) -> Result<Self, FsError> {
        Ok(Self {
            root: KnightFs::mount(dev)?,
        })
    }

    pub fn mount_or_format(dev: D) -> Result<(Self, bool), FsError> {
        let (root, existed) = KnightFs::mount_or_format(dev)?;
        Ok((Self { root }, existed))
    }

    pub fn into_inner(self) -> D {
        self.root.into_inner()
    }

    fn normalize(path: &str) -> Result<&str, FsError> {
        let trimmed = path.trim_start_matches('/');
        if trimmed.is_empty() || trimmed.contains('/') {
            return Err(FsError::InvalidName);
        }
        Ok(trimmed)
    }

    pub fn write(&mut self, path: &str, data: &[u8]) -> Result<(), FsError> {
        let name = Self::normalize(path)?;
        self.root.write_file(name, data)
    }

    pub fn read(&mut self, path: &str, out: &mut [u8]) -> Result<usize, FsError> {
        let name = Self::normalize(path)?;
        self.root.read_file(name, out)
    }

    pub fn stat(&mut self, path: &str) -> Result<FileStat, FsError> {
        let name = Self::normalize(path)?;
        self.root.stat(name)
    }

    pub fn remove(&mut self, path: &str) -> Result<(), FsError> {
        let name = Self::normalize(path)?;
        self.root.delete_file(name)
    }

    pub fn list_root<F: FnMut(&str, u32)>(&mut self, cb: F) -> Result<usize, FsError> {
        self.root.list_files(cb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MemDev(Vec<u8>);

    impl BlockDevice for MemDev {
        fn sector_size(&self) -> usize {
            512
        }
        fn sector_count(&self) -> u64 {
            (self.0.len() / 512) as u64
        }
        fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
            let off = lba as usize * 512;
            buf.copy_from_slice(&self.0[off..off + buf.len()]);
            Ok(())
        }
        fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
            let off = lba as usize * 512;
            self.0[off..off + buf.len()].copy_from_slice(buf);
            Ok(())
        }
    }

    #[test]
    fn vfs_root_paths_and_remount() {
        let mut vfs = Vfs::format_and_mount(MemDev(vec![0; 256 * 512])).unwrap();
        vfs.write("/knight.cfg", b"arch=any").unwrap();
        let mut buf = [0u8; 32];
        let n = vfs.read("knight.cfg", &mut buf).unwrap();
        assert_eq!(&buf[..n], b"arch=any");

        let dev = vfs.into_inner();
        let mut vfs2 = Vfs::mount(dev).unwrap();
        assert_eq!(vfs2.stat("/knight.cfg").unwrap().size_bytes, 8);
    }
}
