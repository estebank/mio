use std::cmp::Ordering;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::RawFd;
use std::path::Path;
use std::{io, mem};

mod datagram;
pub use self::datagram::UnixDatagram;

mod listener;
pub use self::listener::{SocketAddr, UnixListener};

mod stream;
pub use self::stream::UnixStream;

pub fn socket_addr(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let sockaddr = mem::MaybeUninit::<libc::sockaddr_un>::zeroed();

    // This is safe to assume because a `libc::sockaddr_un` filled with `0`
    // bytes is properly initialized.
    //
    // `0` is a valid value for `sockaddr_un::sun_family`; it is
    // `libc::AF_UNSPEC`.
    //
    // `[0; 108]` is a valid value for `sockaddr_un::sun_path`; it begins an
    // abstract path.
    let mut sockaddr = unsafe { sockaddr.assume_init() };

    sockaddr.sun_family = libc::AF_UNIX as libc::sa_family_t;

    let bytes = path.as_os_str().as_bytes();
    match (bytes.get(0), bytes.len().cmp(&sockaddr.sun_path.len())) {
        // Abstract paths don't need a null terminator
        (Some(&0), Ordering::Greater) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path must be no longer than libc::sockaddr_un.sun_path",
            ));
        }
        (_, Ordering::Greater) | (_, Ordering::Equal) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "path must be shorter than libc::sockaddr_un.sun_path",
            ));
        }
        _ => {}
    }

    for (dst, src) in sockaddr.sun_path.iter_mut().zip(bytes.iter()) {
        *dst = *src as libc::c_char;
    }

    let offset = path_offset(&sockaddr);
    let mut socklen = offset + bytes.len();

    match bytes.get(0) {
        // The struct has already been zeroes so the null byte for pathname
        // addresses is already there.
        Some(&0) | None => {}
        Some(_) => socklen += 1,
    }

    Ok((sockaddr, socklen as libc::socklen_t))
}

/// Get the `sun_path` field offset of `sockaddr_un` for the target OS.
///
/// On Linux, this funtion equates to the same value as
/// `size_of::<sa_family_t>()`, but some other implementations include
/// other fields before `sun_path`, so the expression more portably
/// describes the size of the address structure.
pub fn path_offset(sockaddr: &libc::sockaddr_un) -> usize {
    let base = sockaddr as *const _ as usize;
    let path = &sockaddr.sun_path as *const _ as usize;
    path - base
}

fn pair_descriptors(mut fds: [RawFd; 2], flags: i32) -> io::Result<()> {
    #[cfg(not(any(target_os = "ios", target_os = "macos", target_os = "solaris")))]
    let flags = flags | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC;

    syscall!(socketpair(libc::AF_UNIX, flags, 0, fds.as_mut_ptr()))?;

    // Darwin and Solaris don't have SOCK_NONBLOCK or SOCK_CLOEXEC.
    //
    // For platforms that don't support flags in `socket`, the flags must be
    // set through `fcntl`. The `F_SETFL` command sets the `O_NONBLOCK` bit.
    // The `F_SETFD` command sets the `FD_CLOEXEC` bit.
    #[cfg(any(target_os = "ios", target_os = "macos", target_os = "solaris"))]
    {
        syscall!(fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK))?;
        syscall!(fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC))?;
        syscall!(fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK))?;
        syscall!(fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{path_offset, socket_addr};
    use std::path::Path;
    use std::str;

    // Assert `socklen` equals 16 (on Linux):
    //   - 13 bytes for path length
    //   - `path_offset` bytes for the `sun_path` offset (2 on Linux)
    //   - 1 for the null terminator
    #[test]
    fn pathname_address() {
        const PATH: &str = "./foo/bar.txt";
        const PATH_LEN: usize = 13;

        // Pathname addresses do have a null terminator, so `socklen` is
        // expected to be `PATH_LEN` + `offset` + 1.
        let path = Path::new(PATH);
        let (sockaddr, actual) = socket_addr(path).unwrap();
        let offset = path_offset(&sockaddr);
        let expected = PATH_LEN + offset + 1;
        assert_eq!(expected as libc::socklen_t, actual)
    }

    #[test]
    fn abstract_address() {
        const PATH: &[u8] = &[0, 116, 111, 107, 105, 111];
        const PATH_LEN: usize = 6;

        // Abstract addresses do not have a null terminator, so `socklen` is
        // expected to be `PATH_LEN` + `offset`.
        let abstract_path = str::from_utf8(PATH).unwrap();
        let path = Path::new(abstract_path);
        let (sockaddr, actual) = socket_addr(path).unwrap();
        let offset = path_offset(&sockaddr);
        let expected = PATH_LEN + offset;
        assert_eq!(expected as libc::socklen_t, actual)
    }
}