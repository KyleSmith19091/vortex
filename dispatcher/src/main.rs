use dispatcher::network::http_connection::HttpConnection;
use dispatcher::network::server::Server;
use dispatcher::service::controller::Controller;
use dispatcher::service::orchestrator::Orchestrator;
use dispatcher::wasm::fetcher::Fetcher;

use slatedb::object_store::local::LocalFileSystem;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const LISTEN_ADDR: &str = "0.0.0.0:8080";
const MAX_CONNECTIONS: usize = 40;
const MAX_PROCESSES: usize = 4;
const MAX_INVOCATIONS_PER_PROCESS: usize = 10;
const MAX_LOADED_MODULES: usize = 16;
const WASM_STORE_ROOT: &str = "/tmp/vortex/wasm";
/// Path to the runner binary. Uses a relative path from the workspace root.
/// Override with the VORTEX_RUNNER_BIN environment variable.
const DEFAULT_RUNNER_BIN: &str = "./target/debug/runner";

#[tokio::main]
async fn main() {
    let runner_bin = std::env::var("VORTEX_RUNNER_BIN")
        .unwrap_or_else(|_| DEFAULT_RUNNER_BIN.to_string());

    eprintln!("[main] runner_bin={runner_bin}");
    eprintln!("[main] wasm_store={WASM_STORE_ROOT}");
    eprintln!("[main] listen={LISTEN_ADDR}");

    let cancel_token = CancellationToken::new();

    // Channel from server → orchestrator for incoming HTTP connections.
    let (conn_tx, conn_rx) = mpsc::channel::<HttpConnection>(128);

    // Object store for fetching compiled wasm modules.
    let object_store = Box::new(
        LocalFileSystem::new_with_prefix(WASM_STORE_ROOT)
            .expect("failed to create wasm object store"),
    );
    let fetcher = Fetcher::new(object_store);

    // Controller manages runner processes.
    let controller = Controller::new(
        MAX_PROCESSES,
        MAX_INVOCATIONS_PER_PROCESS,
        MAX_LOADED_MODULES,
        runner_bin,
        cancel_token.clone(),
    );

    // Orchestrator receives connections and dispatches invocations.
    let mut orchestrator = Orchestrator::new(conn_rx, fetcher, controller);

    // Shut down on ctrl-c.
    let shutdown_token = cancel_token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        eprintln!("[main] ctrl-c received, cancelling");
        shutdown_token.cancel();
    });

    // Run server and orchestrator concurrently.
    let server = Server::new(MAX_CONNECTIONS, conn_tx);

    tokio::select! {
        _ = server.start_server(LISTEN_ADDR.to_string()) => {},
        _ = orchestrator.run(cancel_token) => {},
    }

    // Orchestrator exited — drop it so the Controller and all Process handles
    // are dropped, which closes the mpsc senders, which causes the writer
    // threads to exit and kill their child processes.
    drop(orchestrator);

    eprintln!("[main] shutdown complete");
}
