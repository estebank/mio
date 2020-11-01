use std::io;
use std::convert::TryInto;
use std::mem::size_of;
use std::net::{self, SocketAddr, SocketAddrV4, SocketAddrV6};
use std::time::Duration;
use std::ptr;
use std::os::windows::io::FromRawSocket;
use std::os::windows::raw::SOCKET as StdSocket; // winapi uses usize, stdlib uses u32/u64.

use winapi::ctypes::{c_char, c_int, c_ushort, c_ulong};
use winapi::shared::ws2def::{SOCKADDR_STORAGE, AF_INET, SOCKADDR_IN};
use winapi::shared::ws2ipdef::SOCKADDR_IN6_LH;
use winapi::shared::mstcpip;

use winapi::shared::minwindef::{BOOL, TRUE, FALSE, DWORD, LPVOID};
use winapi::um::winsock2::{
    self, closesocket, linger, setsockopt, getsockopt, getsockname, PF_INET, PF_INET6, SOCKET, SOCKET_ERROR,
    SOCK_STREAM, SOL_SOCKET, SO_LINGER, SO_REUSEADDR, WSAIoctl, LPWSAOVERLAPPED
};

use crate::sys::windows::net::{init, new_socket, socket_addr};

pub(crate) type TcpSocket = SOCKET;

pub(crate) fn new_v4_socket() -> io::Result<TcpSocket> {
    init();
    new_socket(PF_INET, SOCK_STREAM)
}

pub(crate) fn new_v6_socket() -> io::Result<TcpSocket> {
    init();
    new_socket(PF_INET6, SOCK_STREAM)
}

pub(crate) fn bind(socket: TcpSocket, addr: SocketAddr) -> io::Result<()> {
    use winsock2::bind;

    let (raw_addr, raw_addr_length) = socket_addr(&addr);
    syscall!(
        bind(socket, raw_addr, raw_addr_length),
        PartialEq::eq,
        SOCKET_ERROR
    )?;
    Ok(())
}

pub(crate) fn connect(socket: TcpSocket, addr: SocketAddr) -> io::Result<net::TcpStream> {
    use winsock2::connect;

    let (raw_addr, raw_addr_length) = socket_addr(&addr);

    let res = syscall!(
        connect(socket, raw_addr, raw_addr_length),
        PartialEq::eq,
        SOCKET_ERROR
    );

    match res {
        Err(err) if err.kind() != io::ErrorKind::WouldBlock => {
            Err(err)
        }
        _ => {
            Ok(unsafe { net::TcpStream::from_raw_socket(socket as StdSocket) })
        }
    }
}

pub(crate) fn listen(socket: TcpSocket, backlog: u32) -> io::Result<net::TcpListener> {
    use winsock2::listen;
    use std::convert::TryInto;

    let backlog = backlog.try_into().unwrap_or(i32::max_value());
    syscall!(listen(socket, backlog), PartialEq::eq, SOCKET_ERROR)?;
    Ok(unsafe { net::TcpListener::from_raw_socket(socket as StdSocket) })
}

pub(crate) fn close(socket: TcpSocket) {
    let _ = unsafe { closesocket(socket) };
}

pub(crate) fn set_reuseaddr(socket: TcpSocket, reuseaddr: bool) -> io::Result<()> {
    let val: BOOL = if reuseaddr { TRUE } else { FALSE };

    match unsafe { setsockopt(
        socket,
        SOL_SOCKET,
        SO_REUSEADDR,
        &val as *const _ as *const c_char,
        size_of::<BOOL>() as c_int,
    ) } {
        SOCKET_ERROR => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

pub(crate) fn get_reuseaddr(socket: TcpSocket) -> io::Result<bool> {
    let mut optval: c_char = 0;
    let mut optlen = size_of::<BOOL>() as c_int;

    match unsafe { getsockopt(
        socket,
        SOL_SOCKET,
        SO_REUSEADDR,
        &mut optval as *mut _ as *mut _,
        &mut optlen,
    ) } {
        SOCKET_ERROR => Err(io::Error::last_os_error()),
        _ => Ok(optval != 0),
    }
}

pub(crate) fn get_localaddr(socket: TcpSocket) -> io::Result<SocketAddr> {
    let mut addr: SOCKADDR_STORAGE = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&addr) as c_int;

    match unsafe { getsockname(
        socket,
        &mut addr as *mut _ as *mut _,
        &mut length
    ) } {
        SOCKET_ERROR => Err(io::Error::last_os_error()),
        _ => {
            let storage: *const SOCKADDR_STORAGE = (&addr) as *const _;
            if addr.ss_family as c_int == AF_INET {
                let sock_addr : SocketAddrV4 = unsafe { *(storage as *const SOCKADDR_IN as *const _) };
                Ok(sock_addr.into())
            } else {
                let sock_addr : SocketAddrV6 = unsafe { *(storage as *const SOCKADDR_IN6_LH as *const _) };
                Ok(sock_addr.into())
            }
        },
    }


}

pub(crate) fn set_linger(socket: TcpSocket, dur: Option<Duration>) -> io::Result<()> {
    let val: linger = linger {
        l_onoff: if dur.is_some() { 1 } else { 0 },
        l_linger: dur.map(|dur| dur.as_secs() as c_ushort).unwrap_or_default(),
    };

    match unsafe { setsockopt(
        socket,
        SOL_SOCKET,
        SO_LINGER,
        &val as *const _ as *const c_char,
        size_of::<linger>() as c_int,
    ) } {
        SOCKET_ERROR => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

pub(crate) fn set_keepalive(socket: TcpSocket, dur: Option<Duration>) -> io::Result<()> {
    // Windows takes the keepalive timeout as a u32 of milliseconds.
    let dur_ms = dur.map(|dur| {
        let ms = dur.as_millis();
        ms.try_into().ok().unwrap_or_else(i32::max_value)
    }).unwrap_or(0);

    let keepalive = mstcpip::tcp_keepalive {
        onoff: dur.is_some() as c_ulong,
        keepalivetime: dur_ms as c_ulong,
        keepaliveinterval: dur_ms as c_ulong,
    };

    let mut out = 0;
    match unsafe { WSAIoctl(
        socket,
        mstcpip::SIO_KEEPALIVE_VALS,
        &keepalive as *const _ as *mut _ as LPVOID,
        size_of::<mstcpip::tcp_keepalive> as DWORD,
        ptr::null_mut() as LPVOID,
        0 as DWORD,
        &mut out as *mut _ as LPVOID,
        ptr::null_mut() as LPWSAOVERLAPPED,
        None,
    ) } {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error())
    }
}

pub(crate) fn get_keepalive(socket: TcpSocket) -> io::Result<Option<Duration>> {
    let mut keepalive = mstcpip::tcp_keepalive {
        onoff: 0,
        keepalivetime: 0,
        keepaliveinterval: 0,
    };

    match unsafe { WSAIoctl(
        socket,
        mstcpip::SIO_KEEPALIVE_VALS,
        ptr::null_mut() as LPVOID,
        0,
        &mut keepalive as *mut _ as LPVOID,
        size_of::<mstcpip::tcp_keepalive>() as DWORD,
        ptr::null_mut() as LPVOID,
        ptr::null_mut() as LPWSAOVERLAPPED,
        None,
    ) } {
        0 if keepalive.onoff == 0 || keepalive.keepaliveinterval == 0 => Ok(None),
        0 => Ok(Some(Duration::from_millis(keepalive.keepaliveinterval as u64))),
        _ => Err(io::Error::last_os_error())
    }
}

pub(crate) fn accept(listener: &net::TcpListener) -> io::Result<(net::TcpStream, SocketAddr)> {
    // The non-blocking state of `listener` is inherited. See
    // https://docs.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-accept#remarks.
    listener.accept()
}
