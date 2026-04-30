# Vortex

Vortex is a Rust workspace for running WebAssembly services behind a small dispatcher.

The project is split into two main parts:

- `dispatcher/`: accepts incoming connections, fetches WASM modules, and assigns work to runner processes
- `runner/`: executes WASM invocations and writes logs to `/tmp/vortex`

There are also example modules in `example/` for testing the system.

## Getting Started

Build the workspace:

```bash
cargo build
```

Start the dispatcher:

```bash
cargo run -p dispatcher
```

If needed, point it at a specific runner binary:

```bash
VORTEX_RUNNER_BIN=./target/debug/runner cargo run -p dispatcher
```

## Project Layout

- `dispatcher/` process management, networking, and orchestration
- `runner/` WASM execution and module caching
- `example/rust/` sample Rust WASM module
- `example/go/` sample Go WASI server
