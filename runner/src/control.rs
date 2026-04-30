use std::{
    io,
    os::{
        fd::RawFd,
        unix::net::UnixStream,
    },
};

/// A message received over the control socket from the dispatcher.
pub struct InvocationRequest {
    pub service_id: String,
    pub wasm_bytes: Vec<u8>,
    /// Raw file descriptor for the TCP connection that originated this request.
    pub tcp_fd: RawFd,
}

/// Reads the next invocation request from the control socket.
///
/// Wire format (sent via sendmsg with SCM_RIGHTS carrying the TCP fd):
///   [service_id_len: u32 LE][service_id bytes][wasm_len: u64 LE][wasm bytes]
///
/// Returns `None` when the dispatcher closes its end of the socket.
pub fn recv_invocation(control: &UnixStream) -> io::Result<Option<InvocationRequest>> {
    use std::io::Read;

    // receive the ancillary fd + first chunk via recvmsg 
    let mut cmsg_buf = [0u8; unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize];
    let mut header_buf = [0u8; 4]; // service_id_len

    let mut iov = libc::iovec {
        iov_base: header_buf.as_mut_ptr() as *mut libc::c_void,
        iov_len: header_buf.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_buf.len() as _;

    let n = unsafe { libc::recvmsg(fd_of(control), &mut msg, 0) };
    if n <= 0 {
        if n == 0 {
            return Ok(None); // clean close
        }
        return Err(io::Error::last_os_error());
    }

    // Extract the passed file descriptor from the ancillary data.
    let tcp_fd = extract_fd(&msg)?;

    // We may have received fewer than 4 bytes in the first recvmsg — read the
    // remainder with plain read.
    let received = n as usize;
    if received < 4 {
        let mut stream_ref = control;
        stream_ref.read_exact(&mut header_buf[received..])?;
    }

    let service_id_len = u32::from_le_bytes(header_buf) as usize;

    let mut control_reader = control;

    let mut id_buf = vec![0u8; service_id_len];
    control_reader.read_exact(&mut id_buf)?;
    let service_id = String::from_utf8(id_buf)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut wasm_len_buf = [0u8; 8];
    control_reader.read_exact(&mut wasm_len_buf)?;
    let wasm_len = u64::from_le_bytes(wasm_len_buf) as usize;

    let mut wasm_bytes = vec![0u8; wasm_len];
    control_reader.read_exact(&mut wasm_bytes)?;

    Ok(Some(InvocationRequest {
        service_id,
        wasm_bytes,
        tcp_fd,
    }))
}

/// Send a single error byte back to the dispatcher to signal failure.
pub fn send_error(control: &UnixStream) {
    use std::io::Write;
    let _ = (&*control).write_all(&[1u8]);
}

fn fd_of(stream: &UnixStream) -> RawFd {
    use std::os::fd::AsRawFd;
    stream.as_raw_fd()
}

fn extract_fd(msg: &libc::msghdr) -> io::Result<RawFd> {
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(msg);
        while !cmsg.is_null() {
            let hdr = &*cmsg;
            if hdr.cmsg_level == libc::SOL_SOCKET && hdr.cmsg_type == libc::SCM_RIGHTS {
                let data_ptr = libc::CMSG_DATA(cmsg) as *const RawFd;
                return Ok(std::ptr::read_unaligned(data_ptr));
            }
            cmsg = libc::CMSG_NXTHDR(msg, cmsg);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "no SCM_RIGHTS fd received from dispatcher",
    ))
}
