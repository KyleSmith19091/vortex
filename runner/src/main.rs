mod control;
mod invocation;
mod module_cache;

use std::{
    io::Write,
    os::{fd::FromRawFd, unix::net::UnixStream},
    sync::{Arc, Mutex},
};

use invocation::Invocation;
use module_cache::ModuleCache;
use object_store::local::LocalFileSystem;
use wasmtime::{Config, Engine};

const CONTROL_FD: i32 = 3;
const LOG_ROOT: &str = "/tmp/vortex";
const MAX_CACHED_MODULES: usize = 16;

/// Signal bytes sent back to the dispatcher over the control socket.
const SIGNAL_COMPLETE: u8 = 0x00;

#[tokio::main]
async fn main() {
    eprintln!("[runner] starting, pid={}", std::process::id());

    let control_socket = unsafe { UnixStream::from_raw_fd(CONTROL_FD) };

    // Dup the control socket so async tasks can write completion signals
    // without interfering with the blocking reads in the main loop.
    let write_fd = unsafe { libc::dup(CONTROL_FD) };
    if write_fd < 0 {
        eprintln!("[runner] failed to dup control fd: {}", std::io::Error::last_os_error());
        return;
    }
    let signal_writer = Arc::new(Mutex::new(unsafe { UnixStream::from_raw_fd(write_fd) }));

    eprintln!("[runner] control socket fd={CONTROL_FD} ready (signal writer fd={write_fd})");

    let log_store: Arc<dyn object_store::ObjectStore> =
        Arc::new(LocalFileSystem::new_with_prefix(LOG_ROOT).expect("failed to create log store"));

    let mut config = Config::new();
    config.wasm_component_model(true);
    let engine = Engine::new(&config).expect("failed to create wasmtime engine");
    let module_cache = Arc::new(Mutex::new(ModuleCache::new(engine, MAX_CACHED_MODULES)));

    eprintln!("[runner] ready, waiting for invocations");

    loop {
        let request = match control::recv_invocation(&control_socket) {
            Ok(Some(req)) => {
                eprintln!(
                    "[runner] received invocation service={} wasm_len={} tcp_fd={}",
                    req.service_id,
                    req.wasm_bytes.len(),
                    req.tcp_fd
                );
                req
            }
            Ok(None) => {
                eprintln!("[runner] control socket closed, shutting down");
                break;
            }
            Err(e) => {
                eprintln!("[runner] error reading control socket: {e}");
                control::send_error(&control_socket);
                break;
            }
        };

        let store = Arc::clone(&log_store);
        let cache = Arc::clone(&module_cache);
        let writer = Arc::clone(&signal_writer);
        tokio::spawn(async move {
            let service_id = request.service_id;
            let inv = match Invocation::new(
                service_id.clone(),
                request.wasm_bytes,
                request.tcp_fd,
                store,
                &cache,
            ) {
                Ok(inv) => inv,
                Err(e) => {
                    eprintln!("[runner] failed to create invocation for {service_id}: {e}");
                    send_signal(&writer, SIGNAL_COMPLETE);
                    return;
                }
            };

            let id = inv.invocation_id;
            eprintln!("[runner] executing invocation {id} for {service_id}");
            match inv.execute().await {
                Ok(()) => eprintln!("[runner] invocation {id} completed"),
                Err(e) => eprintln!("[runner] invocation {id} failed: {e}"),
            }

            send_signal(&writer, SIGNAL_COMPLETE);
        });
    }

    eprintln!("[runner] exiting");
}

fn send_signal(writer: &Arc<Mutex<UnixStream>>, signal: u8) {
    if let Ok(mut w) = writer.lock() {
        if let Err(e) = w.write_all(&[signal]) {
            eprintln!("[runner] failed to send signal 0x{signal:02x}: {e}");
        }
    }
}
