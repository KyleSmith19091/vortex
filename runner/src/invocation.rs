use std::{
    net::TcpStream as StdTcpStream,
    os::fd::FromRawFd,
    sync::{Arc, Mutex},
};

use base64::Engine as _;
use bytes::Bytes;
use object_store::{ObjectStore, path::Path};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpStream};
use uuid::Uuid;
use wasmtime::Store;
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime_wasi::{WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView, p2::bindings::CommandPre, p2::pipe::{MemoryInputPipe, MemoryOutputPipe}};
use wasmtime_wasi_http::{WasiHttpCtx, p2::{WasiHttpCtxView, WasiHttpView}};

use crate::module_cache::ModuleCache;

struct HostState {
    wasi: WasiCtx,
    http: WasiHttpCtx,
    table: ResourceTable,
}

impl WasiView for HostState {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl WasiHttpView for HostState {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView {
            ctx: &mut self.http,
            table: &mut self.table,
            hooks: Default::default(),
        }
    }
}

pub struct Invocation {
    pub invocation_id: Uuid,
    pub service_id: String,
    component: Component,
    tcp_stream: TcpStream,
    log_store: Arc<dyn ObjectStore>,
}

impl Invocation {
    /// Create a new invocation, reconstructing the TCP stream from the raw fd
    /// passed by the dispatcher. Compiles the component (or retrieves it from cache).
    pub fn new(
        service_id: String,
        wasm_bytes: Vec<u8>,
        tcp_fd: i32,
        log_store: Arc<dyn ObjectStore>,
        module_cache: &Arc<Mutex<ModuleCache>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        eprintln!("[runner] reconstructing tcp stream from fd={tcp_fd}");

        // Verify the fd is valid before wrapping it.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let fstat_ret = unsafe { libc::fstat(tcp_fd, &mut stat) };
        if fstat_ret != 0 {
            return Err(format!("tcp_fd={tcp_fd} is not a valid fd: {}", std::io::Error::last_os_error()).into());
        }
        eprintln!("[runner] fd={tcp_fd} fstat ok, mode=0o{:o}", stat.st_mode);

        // Check if there's data available with a non-blocking peek.
        let mut peek_buf = [0u8; 1];
        let peek_ret = unsafe {
            libc::recv(tcp_fd, peek_buf.as_mut_ptr() as *mut libc::c_void, 1, libc::MSG_PEEK | libc::MSG_DONTWAIT)
        };
        eprintln!("[runner] fd={tcp_fd} peek returned {peek_ret}");

        let std_stream = unsafe { StdTcpStream::from_raw_fd(tcp_fd) };
        std_stream.set_nonblocking(true)?;
        let tcp_stream = TcpStream::from_std(std_stream)?;

        let component = module_cache
            .lock()
            .map_err(|e| format!("module cache lock poisoned: {e}"))?
            .get_or_compile(&service_id, &wasm_bytes)?;

        Ok(Self {
            invocation_id: Uuid::new_v4(),
            service_id,
            component,
            tcp_stream,
            log_store,
        })
    }

    /// Execute the WASM component and respond to the HTTP request.
    pub async fn execute(mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let id = self.invocation_id;
        let mut log = String::new();

        log.push_str(&format!("invocation_id={id} service={}\n", self.service_id));

        // Read the full HTTP request and decode the base64 body.
        let input = self.read_request_body(&mut log).await?;

        let result = self.run_wasm(&input, &mut log).await;

        // Append captured module output to the log.
        match &result {
            Ok((stdout, stderr)) | Err((_, stdout, stderr)) => {
                if !stdout.is_empty() {
                    log.push_str("--- stdout ---\n");
                    log.push_str(&String::from_utf8_lossy(stdout));
                    if !stdout.ends_with(b"\n") {
                        log.push('\n');
                    }
                }
                if !stderr.is_empty() {
                    log.push_str("--- stderr ---\n");
                    log.push_str(&String::from_utf8_lossy(stderr));
                    if !stderr.ends_with(b"\n") {
                        log.push('\n');
                    }
                }
            }
        }

        match &result {
            Ok(_) => {
                log.push_str("status=ok\n");
                let body = format!("{{\"invocation_id\":\"{id}\"}}");
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body,
                );
                self.tcp_stream.write_all(response.as_bytes()).await?;
            }
            Err((e, _, _)) => {
                log.push_str(&format!("status=error error={e}\n"));
                let body = format!("{{\"invocation_id\":\"{id}\",\"error\":\"{e}\"}}");
                let response = format!(
                    "HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body,
                );
                self.tcp_stream.write_all(response.as_bytes()).await?;
            }
        }

        self.tcp_stream.shutdown().await?;

        // Persist logs to object storage.
        let log_path = Path::from(format!("logs/{}/{id}.log", self.service_id));
        self.log_store.put(&log_path, Bytes::from(log).into()).await?;

        result.map(|_| ()).map_err(|(e, _, _)| e)
    }

    /// Read the raw HTTP request from the TCP stream, extract the body, and
    /// decode it from base64. The dispatcher only peeked at the bytes so the
    /// full request is still in the receive buffer.
    async fn read_request_body(
        &mut self,
        log: &mut String,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 4096];

        // Read until we have the full headers (terminated by \r\n\r\n).
        let header_end = loop {
            let n = self.tcp_stream.read(&mut tmp).await?;
            if n == 0 {
                return Err("connection closed before headers received".into());
            }
            buf.extend_from_slice(&tmp[..n]);

            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
        };

        let headers = std::str::from_utf8(&buf[..header_end])
            .map_err(|_| "headers are not valid UTF-8")?;

        let content_length: usize = headers
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                if key.trim().eq_ignore_ascii_case("content-length") {
                    value.trim().parse().ok()
                } else {
                    None
                }
            })
            .ok_or("missing Content-Length header")?;

        // Body starts after \r\n\r\n.
        let body_start = header_end + 4;
        let already_read = buf.len() - body_start;

        // Read remaining body bytes if we don't have them all yet.
        if already_read < content_length {
            let remaining = content_length - already_read;
            let mut body_buf = vec![0u8; remaining];
            self.tcp_stream.read_exact(&mut body_buf).await?;
            buf.extend_from_slice(&body_buf);
        }

        let body = &buf[body_start..body_start + content_length];

        let b64_str = std::str::from_utf8(body)
            .map_err(|_| "request body is not valid UTF-8")?;

        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64_str)
            .map_err(|e| format!("invalid base64 body: {e}"))?;

        log.push_str(&format!("read {} body bytes ({} decoded)\n", b64_str.len(), decoded.len()));
        Ok(decoded)
    }

    /// Instantiate and run the pre-compiled WASM component via the p2
    /// component model with WASI + HTTP support. The decoded request body
    /// is piped to the component's stdin.
    ///
    /// Returns (stdout_bytes, stderr_bytes) on success, or
    /// (error, stdout_bytes, stderr_bytes) on failure.
    async fn run_wasm(
        &self,
        input: &[u8],
        log: &mut String,
    ) -> Result<(Vec<u8>, Vec<u8>), (Box<dyn std::error::Error + Send + Sync>, Vec<u8>, Vec<u8>)> {
        let engine = self.component.engine();

        let stdin = MemoryInputPipe::new(Bytes::copy_from_slice(input));
        let stdout_pipe = MemoryOutputPipe::new(usize::MAX);
        let stderr_pipe = MemoryOutputPipe::new(usize::MAX);

        let stdout_capture = stdout_pipe.clone();
        let stderr_capture = stderr_pipe.clone();

        let wasi = WasiCtxBuilder::new()
            .stdin(stdin)
            .stdout(stdout_pipe)
            .stderr(stderr_pipe)
            .build();

        let state = HostState {
            wasi,
            http: WasiHttpCtx::new(),
            table: ResourceTable::new(),
        };

        let mut store = Store::new(engine, state);

        let mut linker: Linker<HostState> = Linker::new(engine);

        let collect = || {
            (
                stdout_capture.contents().to_vec(),
                stderr_capture.contents().to_vec(),
            )
        };

        if let Err(e) = wasmtime_wasi::p2::add_to_linker_async(&mut linker) {
            let (out, err) = collect();
            return Err((format!("add wasi to linker: {e}").into(), out, err));
        }
        if let Err(e) = wasmtime_wasi_http::p2::add_only_http_to_linker_async(&mut linker) {
            let (out, err) = collect();
            return Err((format!("add http to linker: {e}").into(), out, err));
        }

        log.push_str("component instantiating\n");

        let instance_pre = match linker.instantiate_pre(&self.component) {
            Ok(p) => p,
            Err(e) => {
                let (out, err) = collect();
                return Err((format!("instantiate_pre: {e}").into(), out, err));
            }
        };
        let pre = match CommandPre::new(instance_pre) {
            Ok(pre) => pre,
            Err(e) => {
                let (out, err) = collect();
                return Err((format!("CommandPre::new: {e}").into(), out, err));
            }
        };

        let command = match pre.instantiate_async(&mut store).await {
            Ok(c) => c,
            Err(e) => {
                let (out, err) = collect();
                return Err((format!("instantiate: {e}").into(), out, err));
            }
        };

        log.push_str("calling wasi:cli/run\n");

        match command.wasi_cli_run().call_run(&mut store).await {
            Ok(Ok(())) => {
                log.push_str("run returned ok\n");
                let (out, err) = collect();
                Ok((out, err))
            }
            Ok(Err(())) => {
                let (out, err) = collect();
                Err(("wasm component returned error from wasi:cli/run".into(), out, err))
            }
            Err(e) => {
                let (out, err) = collect();
                Err((format!("call_run: {e}").into(), out, err))
            }
        }
    }
}
