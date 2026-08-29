//! Metadata preservation (permissions, mtime/atime, xattrs).
//!
//! All operations are best-effort: failures are logged but not fatal, since
//! a partial metadata copy is still a successful file transfer.

use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::error::EngineResult;

/// Snapshot of metadata we'll restore on the destination.
#[derive(Debug, Clone)]
pub struct MetadataSnapshot {
    pub mode: u32,
    pub mtime: i64,
    pub atime: i64,
}

impl MetadataSnapshot {
    pub fn capture(src: &Path) -> EngineResult<Self> {
        let md = fs::symlink_metadata(src)?;
        let mode = md.permissions().mode();
        let mtime = mtime_of(&md);
        let atime = atime_of(&md);
        Ok(Self { mode, mtime, atime })
    }

    /// Best-effort restore — errors are returned to the caller.
    pub fn restore(&self, dst: &Path) -> std::io::Result<()> {
        fs::set_permissions(dst, fs::Permissions::from_mode(self.mode))?;
        set_file_times(dst, self.atime, self.mtime)
    }
}

fn mtime_of(md: &fs::Metadata) -> i64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn atime_of(md: &fs::Metadata) -> i64 {
    md.accessed()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn set_file_times(path: &Path, atime: i64, mtime: i64) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has NUL"))?;
    let times = [
        libc_timespec {
            tv_sec: atime,
            tv_nsec: 0,
        },
        libc_timespec {
            tv_sec: mtime,
            tv_nsec: 0,
        },
    ];
    // SAFETY: c_path is a valid C string for the duration of the call.
    let res = unsafe { libc_utimensat(-1, c_path.as_ptr(), times.as_ptr(), 0) };
    if res == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn set_file_times(_path: &Path, _atime: i64, _mtime: i64) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
#[repr(C)]
struct libc_timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[cfg(unix)]
extern "C" {
    #[link_name = "utimensat"]
    fn libc_utimensat(
        dirfd: i32,
        pathname: *const std::os::raw::c_char,
        times: *const libc_timespec,
        flags: i32,
    ) -> i32;
}

/// Copy xattrs from `src` to `dst` (best-effort). Returns the number of
/// attributes copied.
#[cfg(target_os = "linux")]
pub fn copy_xattrs(src: &Path, dst: &Path) -> std::io::Result<usize> {
    let names = match xattr::list(src) {
        Ok(it) => it,
        Err(e) if e.raw_os_error() == Some(LIBC_ENODATA) => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut count = 0;
    for name in names {
        // xattr's public API takes `AsRef<OsStr>` for the attribute name.
        // `OsStrExt::from_bytes` is well-defined for raw OS bytes (which
        // is what `OsString::as_encoded_bytes` already returned).
        let name_os =
            std::os::unix::ffi::OsStrExt::from_bytes(name.as_encoded_bytes());
        let value = match xattr::get(src, name_os) {
            Ok(Some(v)) => v,
            Ok(None) => continue,
            Err(_) => continue,
        };
        if xattr::set(dst, name_os, &value).is_ok() {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(not(target_os = "linux"))]
pub fn copy_xattrs(_src: &Path, _dst: &Path) -> std::io::Result<usize> {
    Ok(0)
}

#[cfg(target_os = "linux")]
const LIBC_ENODATA: i32 = 61;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn snapshot_round_trip_permissions() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("a.txt");
        let dst = dir.path().join("b.txt");
        let mut f = std::fs::File::create(&src).unwrap();
        f.write_all(b"hi").unwrap();
        drop(f);
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o600)).unwrap();

        let snap = MetadataSnapshot::capture(&src).unwrap();
        std::fs::File::create(&dst).unwrap();
        snap.restore(&dst).unwrap();

        let got = std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(got, 0o600);
    }
}
