use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::{
        fd::{AsRawFd, IntoRawFd, RawFd},
        unix::net::UnixStream,
    },
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use std::os::unix::process::CommandExt;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// ControlMessage represents commands sent to a running process over its control socket.
pub enum ControlMessage {
    /// Invoke a service with the given WASM module bytes.
    /// The tcp_fd is the raw file descriptor of the HTTP connection that
    /// originated the request — it is passed to the runner via SCM_RIGHTS.
    Invocation {
        service_id: String,
        wasm_bytes: Vec<u8>,
        tcp_fd: RawFd,
    },
}

/// Signal bytes sent back from the runner over the control socket.
/// 0x00 = invocation completed, 0x01 = fatal error.
const SIGNAL_COMPLETE: u8 = 0x00;
const SIGNAL_ERROR: u8 = 0x01;

/// Process is a running unix process.
pub struct Process {
    /// PID of the process.
    pub pid: i32,

    /// control_fd is the file descriptor of the unix socket used to communicate with the process.
    pub control_fd: i32,

    /// loaded_modules is a bounded FIFO queue of unique service IDs with pre-warmed WASM runtimes.
    loaded_modules: VecDeque<String>,

    /// max_loaded_modules is the maximum number of cached modules on this process.
    max_loaded_modules: usize,

    /// max_invocation_capacity is the maximum number of invocations allowed on a process.
    pub max_invocation_capacity: usize,

    /// Shared counter of currently running invocations. Incremented by the
    /// controller when scheduling, decremented by the writer thread when
    /// it reads a completion signal from the runner.
    running_invocations: Arc<AtomicUsize>,

    /// sender for dispatching control messages to the process's write thread.
    pub sender: mpsc::Sender<ControlMessage>,
}

impl Process {
    fn kill_child(child: &mut Child) {
        eprintln!("[dispatcher] killing child process pid={}", child.id());
        let _ = child.kill();
        let _ = child.wait();
    }

    pub fn has_capacity(&self) -> bool {
        self.running_invocations.load(Ordering::Acquire) < self.max_invocation_capacity
    }

    pub fn running_invocations(&self) -> usize {
        self.running_invocations.load(Ordering::Acquire)
    }

    pub fn increment_invocations(&self) {
        self.running_invocations.fetch_add(1, Ordering::Release);
    }

    pub fn has_runtime_loaded(&self, service_id: &str) -> bool {
        self.loaded_modules.iter().any(|id| id == service_id)
    }

    /// Track a module as loaded on this process. If the module is already
    /// present it gets moved to the back (most recent). When the queue is
    /// full the oldest entry is evicted first.
    pub fn track_module(&mut self, service_id: String) {
        if let Some(pos) = self.loaded_modules.iter().position(|id| id == &service_id) {
            self.loaded_modules.remove(pos);
        }

        if self.loaded_modules.len() == self.max_loaded_modules {
            self.loaded_modules.pop_front();
        }

        self.loaded_modules.push_back(service_id);
    }

    /// Spawn the runner binary, bind the socket to fd 3 via dup2, and start a
    /// background thread that forwards ControlMessages over the socket.
    ///
    /// Returns the fully constructed Process or an error.
    pub fn run(
        runner_bin: &str,
        max_invocation_capacity: usize,
        max_loaded_modules: usize,
        cancel_token: CancellationToken,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        eprintln!("[dispatcher] spawning runner: {runner_bin}");

        let (host_socket, runner_socket) = UnixStream::pair()?;

        let runner_fd: i32 = runner_socket.into_raw_fd();

        let child = unsafe {
            Command::new(runner_bin)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .pre_exec(move || {
                    if runner_fd != 3 {
                        if libc::dup2(runner_fd, 3) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        libc::close(runner_fd);
                    }
                    Ok(())
                })
                .spawn()?
        };

        let pid = child.id() as i32;
        let control_fd = host_socket.as_raw_fd();

        eprintln!("[dispatcher] runner spawned pid={pid}");

        let (tx, mut rx) = mpsc::channel::<ControlMessage>(64);
        let running = Arc::new(AtomicUsize::new(0));
        let running_thread = Arc::clone(&running);

        let cancel = cancel_token.clone();
        let mut socket = host_socket;
        let mut child = child;

        thread::spawn(move || {
            loop {
                if cancel.is_cancelled() {
                    eprintln!("[dispatcher] cancel token fired, killing pid={}", child.id());
                    Self::kill_child(&mut child);
                    break;
                }

                let msg = match rx.blocking_recv() {
                    Some(msg) => msg,
                    None => {
                        eprintln!("[dispatcher] channel closed, killing pid={}", child.id());
                        Self::kill_child(&mut child);
                        break;
                    }
                };

                match msg {
                    ControlMessage::Invocation { service_id, wasm_bytes, tcp_fd } => {
                        eprintln!(
                            "[dispatcher] sending invocation service={service_id} \
                             wasm_len={} tcp_fd={tcp_fd} to pid={}",
                            wasm_bytes.len(),
                            child.id()
                        );

                        let id_bytes = service_id.as_bytes();
                        let header = (id_bytes.len() as u32).to_le_bytes();

                        if let Err(e) = send_with_fd(socket.as_raw_fd(), &header, tcp_fd) {
                            eprintln!("[dispatcher] sendmsg failed: {e}");
                            unsafe { libc::close(tcp_fd); }
                            Self::kill_child(&mut child);
                            break;
                        }
                        unsafe { libc::close(tcp_fd); }

                        if let Err(e) = socket.write_all(id_bytes) {
                            eprintln!("[dispatcher] write service_id failed: {e}");
                            Self::kill_child(&mut child);
                            break;
                        }
                        let len = (wasm_bytes.len() as u64).to_le_bytes();
                        if let Err(e) = socket.write_all(&len) {
                            eprintln!("[dispatcher] write wasm_len failed: {e}");
                            Self::kill_child(&mut child);
                            break;
                        }
                        if let Err(e) = socket.write_all(&wasm_bytes) {
                            eprintln!("[dispatcher] write wasm_bytes failed: {e}");
                            Self::kill_child(&mut child);
                            break;
                        }

                        eprintln!("[dispatcher] invocation sent to pid={}", child.id());
                    }
                }

                // Drain all available signal bytes from the runner (non-blocking).
                if drain_signals(&mut socket, &running_thread, &mut child) {
                    break;
                }
            }
        });

        Ok(Self {
            pid,
            control_fd,
            loaded_modules: VecDeque::new(),
            max_loaded_modules,
            max_invocation_capacity,
            running_invocations: running,
            sender: tx,
        })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        eprintln!("[dispatcher] dropping process pid={}", self.pid);
    }
}

/// Read all available signal bytes from the runner socket (non-blocking).
/// Returns `true` if the process should be killed (socket closed or fatal error).
fn drain_signals(socket: &mut UnixStream, running: &AtomicUsize, child: &mut Child) -> bool {
    socket.set_nonblocking(true).ok();

    let mut buf = [0u8; 64];
    loop {
        match socket.read(&mut buf) {
            Ok(0) => {
                eprintln!("[dispatcher] runner pid={} closed socket", child.id());
                Process::kill_child(child);
                return true;
            }
            Ok(n) => {
                for &b in &buf[..n] {
                    match b {
                        SIGNAL_COMPLETE => {
                            let prev = running.fetch_sub(1, Ordering::Release);
                            eprintln!(
                                "[dispatcher] runner pid={} completed invocation (running: {} -> {})",
                                child.id(), prev, prev.saturating_sub(1)
                            );
                        }
                        SIGNAL_ERROR => {
                            eprintln!("[dispatcher] runner pid={} sent fatal error", child.id());
                            Process::kill_child(child);
                            return true;
                        }
                        other => {
                            eprintln!(
                                "[dispatcher] runner pid={} sent unknown signal byte: 0x{other:02x}",
                                child.id()
                            );
                        }
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                break; // no more data
            }
            Err(e) => {
                eprintln!("[dispatcher] read from runner pid={} failed: {e}", child.id());
                Process::kill_child(child);
                return true;
            }
        }
    }

    socket.set_nonblocking(false).ok();
    false
}

/// Send `data` over a unix socket, attaching `fd` as SCM_RIGHTS ancillary data.
fn send_with_fd(socket_fd: RawFd, data: &[u8], fd: RawFd) -> std::io::Result<()> {
    let cmsg_space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<i32>() as u32) } as usize;
    let mut cmsg_buf = vec![0u8; cmsg_space];

    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };

    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cmsg_buf.as_mut_ptr() as *mut libc::c_void;
    msg.msg_controllen = cmsg_space as _;

    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as u32) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg) as *mut i32, fd);
    }

    let n = unsafe { libc::sendmsg(socket_fd, &msg, 0) };
    if n < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
