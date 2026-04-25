use std::os::fd::AsRawFd;
use std::sync::mpsc;

use dispatcher::network::http_connection::{HttpConnection, HttpConnectionType};
use dispatcher::network::server::Server;

#[tokio::main]
async fn main() {
    let (tx, rx) = mpsc::channel::<HttpConnection>();

    std::thread::spawn(move || {
        while let Ok(conn) = rx.recv() {
            let proto = match conn.connection_type {
                HttpConnectionType::HTTP1 => "HTTP/1.1",
                HttpConnectionType::HTTP2 => "HTTP/2",
                HttpConnectionType::UNSPECIFIED => "UNSPECIFIED",
            };
            let fd = conn.tcp_stream.as_raw_fd();
            println!("--- connection ---");
            println!("  protocol: {proto}");
            println!("  path:     {}", conn.raw_path);
            println!("  service:  {}", conn.service_name);
            println!("  fd:       {fd}");
        }
    });

    let server = Server::new(40, tx);
    server.start_server("0.0.0.0:8080".to_string()).await;
}
