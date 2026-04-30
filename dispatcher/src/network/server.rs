use std::sync::Arc;

use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
};

use crate::network::http_connection::HttpConnection;

pub struct Server {
    semaphore: Arc<Semaphore>,
    http_connection_queue: mpsc::Sender<HttpConnection>,
}

impl Server {
    pub fn new(max_num_connections: usize, http_connection_queue: mpsc::Sender<HttpConnection>) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_num_connections)),
            http_connection_queue,
        }
    }

    pub async fn start_server(&self, address: String) {
        println!("Starting the server");
        let listener = TcpListener::bind(address.clone()).await;
        match listener {
            Ok(listener) => {
                println!("Server started on {address}");
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let connection_clone = self.semaphore.clone();
                    let connection_queue_clone = self.http_connection_queue.clone();
                    tokio::spawn(async move {
                        Self::handle_connection(stream, connection_clone, connection_queue_clone)
                            .await;
                    });
                }
            }
            Err(e) => {
                println!("error binding to address {}: {}", address, e);
                return;
            }
        }
    }

    async fn handle_connection(
        mut stream: TcpStream,
        semaphore: Arc<Semaphore>,
        connection_sender: mpsc::Sender<HttpConnection>,
    ) {
        match semaphore.acquire().await {
            Ok(_) => {
                let connection = match HttpConnection::from_tcp_stream(stream).await {
                    Ok(c) => c,
                    Err(e) => {
                        println!("error handling tcp connection as http connection: {e}");
                        return;
                    }
                };
                if connection_sender.send(connection).await.is_err() {
                    println!("could not handoff new http connection in connection_sender queue");
                }
            }
            Err(_) => {
                println!("connection limit reached, bouncing connection");
                match stream
                    .write("server connection limit reached".as_bytes())
                    .await
                {
                    Ok(_) => {}
                    Err(e) => {
                        println!(
                            "could not write to client to indicate server connection limit was reached: {}",
                            e
                        );
                    }
                }
            }
        }
    }
}
