use std::os::fd::IntoRawFd;

use tokio::{io::AsyncWriteExt, select, sync::mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    network::http_connection::HttpConnection,
    service::controller::Controller,
    wasm::fetcher::Fetcher,
};

pub struct Orchestrator {
    connection_receiver: mpsc::Receiver<HttpConnection>,
    wasm_fetcher: Fetcher,
    controller: Controller,
}

impl Orchestrator {
    pub fn new(
        connection_receiver: mpsc::Receiver<HttpConnection>,
        wasm_fetcher: Fetcher,
        controller: Controller,
    ) -> Self {
        Self {
            connection_receiver,
            wasm_fetcher,
            controller,
        }
    }

    pub async fn run(&mut self, cancellation_token: CancellationToken) {
        eprintln!("[orchestrator] started");
        loop {
            select! {
                biased;
                _ = cancellation_token.cancelled() => {
                    eprintln!("[orchestrator] cancelled, shutting down");
                    return;
                }
                connection = self.connection_receiver.recv() => {
                    let connection = match connection {
                        Some(c) => c,
                        None => {
                            eprintln!("[orchestrator] connection channel closed");
                            return;
                        }
                    };

                    eprintln!(
                        "[orchestrator] received {:?} request: {}/{}",
                        connection.kind, connection.version, connection.service
                    );

                    match connection.kind {
                        crate::network::http_connection::RequestKind::Invocation => {
                            match self.handle_invocation(connection).await {
                                Ok(_) => {},
                                Err(e) => {
                                    eprintln!("[orchestrator] invocation error: {e}");
                                },
                            }
                        },
                        crate::network::http_connection::RequestKind::Metadata => {
                            self.handle_metadata().await;
                        },
                    }
                }
            }
        }
    }

    pub async fn handle_invocation(
        &mut self,
        mut http_connection: HttpConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let service_id = format!("{}/{}", http_connection.version, http_connection.service);
        eprintln!("[orchestrator] fetching wasm for {service_id}");

        let result = self
            .wasm_fetcher
            .fetch_wasm_bytes(&http_connection.version, &http_connection.service)
            .await?;

        let wasm_bytes = match result {
            Some(bytes) => {
                eprintln!("[orchestrator] fetched {} bytes for {service_id}", bytes.len());
                bytes
            }
            None => {
                eprintln!("[orchestrator] wasm not found for {service_id}");
                http_connection
                    .tcp_stream
                    .write(
                        format!(
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await?;
                return Ok(());
            }
        };

        // Convert the tokio TcpStream → std TcpStream → raw fd so it can be
        // sent to the runner process via SCM_RIGHTS. into_std() deregisters the
        // fd from tokio's reactor so it won't be closed on drop.
        let std_stream = http_connection.tcp_stream.into_std()?;
        let tcp_fd = std_stream.into_raw_fd();

        eprintln!("[orchestrator] scheduling invocation for {service_id} tcp_fd={tcp_fd}");

        if let Err(e) = self.controller.schedule_invocation(service_id, wasm_bytes.to_vec(), tcp_fd).await {
            eprintln!("[orchestrator] schedule_invocation failed: {e}");
            unsafe { libc::close(tcp_fd); }
            return Err(e);
        }

        eprintln!("[orchestrator] invocation scheduled");
        Ok(())
    }

    pub async fn handle_metadata(&self) {}
}
