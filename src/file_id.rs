//! What an open file is, asked of its handle: the file system object, whatever path led to it.

use std::fs::File;
use std::io;

/// The identity of a file and the number of its names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileId {
    /// The device on Unix, the volume serial number on Windows.
    pub(crate) volume: u64,
    /// The inode on Unix, the file index on Windows.
    pub(crate) index: u64,
    /// The number of hard links.
    pub(crate) links: u64,
}

#[cfg(unix)]
pub(crate) fn of(file: &File) -> io::Result<FileId> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(FileId {
        volume: metadata.dev(),
        index: metadata.ino(),
        links: metadata.nlink(),
    })
}

#[cfg(windows)]
// std has no stable accessor for these fields
#[allow(unsafe_code)]
pub(crate) fn of(file: &File) -> io::Result<FileId> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    // SAFETY: the struct is plain data, and the call gets our own open handle and that struct.
    let info = unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        info
    };
    Ok(FileId {
        volume: info.dwVolumeSerialNumber.into(),
        index: u64::from(info.nFileIndexHigh) << 32 | u64::from(info.nFileIndexLow),
        links: info.nNumberOfLinks.into(),
    })
}
