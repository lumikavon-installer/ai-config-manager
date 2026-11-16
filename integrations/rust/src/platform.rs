use crate::{err, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

pub(crate) type Identity = (u64, u64);

#[cfg(unix)]
pub(crate) fn identity(path: &Path) -> Result<Identity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(path).map_err(|_| err("SYNC_CONFIG"))?;
    Ok((metadata.dev(), metadata.ino()))
}

pub(crate) fn safe_path(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    // Reject junctions and every other reparse point too.
                    if metadata.file_attributes() & 0x400 != 0 {
                        return Err(err("SYNC_CONFIG"));
                    }
                }
                if metadata.file_type().is_symlink() || (ancestor != path && !metadata.is_dir()) {
                    return Err(err("SYNC_CONFIG"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(err("SYNC_CONFIG")),
        }
    }
    Ok(())
}

pub(crate) fn private_dir(path: &Path) -> Result<()> {
    safe_path(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(|_| err("SYNC_CONFIG"))?;
    }
    #[cfg(windows)]
    {
        let parent = path.parent().ok_or_else(|| err("SYNC_CONFIG"))?;
        fs::create_dir_all(parent).map_err(|_| err("SYNC_CONFIG"))?;
        win::create_private_dir(path)?;
    }
    safe_path(path)?;
    restrict(path, true)
}

#[cfg(unix)]
pub(crate) fn restrict(path: &Path, directory: bool) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(path).map_err(|_| err("SYNC_CONFIG"))?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(err("SYNC_CONFIG"));
    }
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
    .map_err(|_| err("SYNC_CONFIG"))
}

pub(crate) fn safe_file(path: &Path) -> Result<()> {
    safe_path(path)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err(err("SYNC_CONFIG"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() } {
                    return Err(err("SYNC_CONFIG"));
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(err("SYNC_CONFIG")),
    }
}

pub(crate) fn read(path: &Path, limit: usize) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>> {
    safe_file(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(err("SYNC_CONFIG")),
    };
    let mut data = zeroize::Zeroizing::new(Vec::new());
    file.take(limit as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| err("SYNC_CONFIG"))?;
    if data.len() > limit {
        return Err(err("SYNC_CONFIG"));
    }
    Ok(Some(data))
}

#[cfg(unix)]
pub(crate) fn open_private(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    safe_file(path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| err("SYNC_CONFIG"))?;
    restrict(path, false)?;
    Ok(file)
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    // The instance lock serializes this private, metadata-only staging file.
    let stage = path.with_extension("stage");
    safe_file(path)?;
    let mut file = open_private(&stage)?;
    file.set_len(0)
        .and_then(|_| file.write_all(bytes))
        .and_then(|_| file.sync_all())
        .map_err(|_| err("SYNC_CONFIG"))?;
    drop(file);
    #[cfg(unix)]
    {
        fs::rename(&stage, path).map_err(|_| err("SYNC_CONFIG"))?;
        File::open(path.parent().ok_or_else(|| err("SYNC_CONFIG"))?)
            .and_then(|file| file.sync_all())
            .map_err(|_| err("SYNC_CONFIG"))?;
    }
    #[cfg(windows)]
    win::replace(&stage, path)?;
    Ok(())
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::{
        os::windows::{ffi::OsStrExt, io::FromRawHandle},
        ptr,
    };
    use windows_sys::Win32::{
        Foundation::*,
        Security::{Authorization::*, *},
        Storage::FileSystem::*,
        System::Threading::*,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    pub(crate) fn identity(path: &Path) -> Result<Identity> {
        unsafe {
            let handle = CreateFileW(
                wide(path).as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(err("SYNC_CONFIG"));
            }
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            let ok = GetFileInformationByHandle(handle, &mut info);
            CloseHandle(handle);
            if ok == 0 {
                return Err(err("SYNC_CONFIG"));
            }
            Ok((
                info.dwVolumeSerialNumber.into(),
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ))
        }
    }

    struct Descriptor(PSECURITY_DESCRIPTOR);
    impl Drop for Descriptor {
        fn drop(&mut self) {
            unsafe { LocalFree(self.0) };
        }
    }
    fn descriptor() -> Result<Descriptor> {
        unsafe {
            let mut token = ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(err("SYNC_CONFIG"));
            }
            let mut needed = 0;
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
            let mut buffer = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
            let ok = GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            );
            CloseHandle(token);
            if ok == 0 {
                return Err(err("SYNC_CONFIG"));
            }
            let user = &*(buffer.as_ptr() as *const TOKEN_USER);
            let mut sid = ptr::null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
                return Err(err("SYNC_CONFIG"));
            }
            let mut length = 0;
            while *sid.add(length) != 0 {
                length += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, length));
            LocalFree(sid.cast());
            let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;{text})")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            ) == 0
            {
                return Err(err("SYNC_CONFIG"));
            }
            Ok(Descriptor(descriptor))
        }
    }

    pub(super) fn create_private_dir(path: &Path) -> Result<()> {
        let descriptor = descriptor()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        unsafe {
            if CreateDirectoryW(wide(path).as_ptr(), &attributes) == 0
                && GetLastError() != ERROR_ALREADY_EXISTS
            {
                return Err(err("SYNC_CONFIG"));
            }
        }
        Ok(())
    }

    pub(crate) fn restrict(path: &Path, _: bool) -> Result<()> {
        let descriptor = descriptor()?;
        unsafe {
            if SetFileSecurityW(
                wide(path).as_ptr(),
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                descriptor.0,
            ) == 0
            {
                return Err(err("SYNC_CONFIG"));
            }
        }
        Ok(())
    }

    pub(crate) fn open_private(path: &Path) -> Result<File> {
        safe_file(path)?;
        let descriptor = descriptor()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        unsafe {
            let handle = CreateFileW(
                wide(path).as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                &attributes,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            );
            if handle == INVALID_HANDLE_VALUE {
                return Err(err("SYNC_CONFIG"));
            }
            let file = File::from_raw_handle(handle);
            restrict(path, false)?;
            Ok(file)
        }
    }

    pub(super) fn replace(from: &Path, to: &Path) -> Result<()> {
        unsafe {
            if MoveFileExW(
                wide(from).as_ptr(),
                wide(to).as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            ) == 0
            {
                return Err(err("SYNC_CONFIG"));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
pub(crate) use win::{identity, open_private, restrict};
