use std::io::Read;

use wasi::http::outgoing_handler;
use wasi::http::types::{
    Fields, OutgoingRequest, RequestOptions, Scheme,
};

fn main() {
    // Read the URL to fetch from stdin (piped by the runner from the decoded
    // base64 request body).
    let mut url = String::new();
    std::io::stdin().read_to_string(&mut url).expect("failed to read stdin");
    let url = url.trim();

    eprintln!("[test-module] making GET request to: {url}");

    // Parse the URL into scheme, authority, and path.
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (Scheme::Https, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (Scheme::Http, rest)
    } else {
        eprintln!("[test-module] invalid URL, must start with http:// or https://");
        std::process::exit(1);
    };

    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    // Build the outgoing request.
    let headers = Fields::new();
    let request = OutgoingRequest::new(headers);
    request.set_method(&wasi::http::types::Method::Get).unwrap();
    request.set_scheme(Some(&scheme)).unwrap();
    request.set_authority(Some(authority)).unwrap();
    request.set_path_with_query(Some(path)).unwrap();

    // Send with default options.
    let options = RequestOptions::new();
    let future_response = outgoing_handler::handle(request, Some(options)).expect("failed to send request");

    // Block until the response arrives.
    let incoming_response = loop {
        if let Some(result) = future_response.get() {
            break result.unwrap().unwrap();
        }
        future_response.subscribe().block();
    };

    let status = incoming_response.status();
    eprintln!("[test-module] response status: {status}");

    // Read the response body.
    let body = incoming_response.consume().expect("failed to consume body");
    let body_stream = body.stream().expect("failed to get body stream");

    let mut response_bytes = Vec::new();
    loop {
        match body_stream.read(65536) {
            Ok(chunk) => response_bytes.extend_from_slice(&chunk),
            Err(wasi::io::streams::StreamError::Closed) => break,
            Err(e) => {
                eprintln!("[test-module] error reading body: {e:?}");
                std::process::exit(1);
            }
        }
    }

    // Write status + body to stdout (captured by the runner and persisted in logs).
    println!("status: {status}");
    println!("body_length: {}", response_bytes.len());
    println!("body: {}", String::from_utf8_lossy(&response_bytes));
}
