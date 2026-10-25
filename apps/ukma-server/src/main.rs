//! ukma-server — UKMA HTTP entry point (§31.2).
//!
//! Modes:
//! - `ukma-server` / `ukma-server serve [--addr 127.0.0.1:8080]` — serve the API
//! - `ukma-server --manifest` — print `tool-manifest.json`
//! - `ukma-server --health` — print the health payload
//! - `ukma-server --operations` — print the exposed operation ids
//!
//! The runtime is Rust with no async runtime dependency: one thread, one request
//! per connection, `Connection: close`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use ukma_server::{HttpRequest, Server};

const DEFAULT_ADDR: &str = "127.0.0.1:8080";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode = "serve".to_string();
    let mut addr = DEFAULT_ADDR.to_string();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--manifest" => mode = "manifest".into(),
            "--health" => mode = "health".into(),
            "--operations" => mode = "operations".into(),
            "serve" => mode = "serve".into(),
            "--addr" => {
                index += 1;
                match args.get(index) {
                    Some(value) => addr = value.clone(),
                    None => {
                        eprintln!("ukma-server: --addr needs a value");
                        std::process::exit(2);
                    }
                }
            }
            "--help" | "-h" => mode = "help".into(),
            unknown => {
                eprintln!("ukma-server: unknown argument `{unknown}`; try --help");
                std::process::exit(2);
            }
        }
        index += 1;
    }

    let server = Server::in_memory();
    match mode.as_str() {
        "manifest" => println!("{}", server.manifest()),
        "health" => println!("{}", ukma_mcp::rest::health_json()),
        "operations" => {
            for operation in ukma_server::exposed_operations() {
                println!("{operation}");
            }
        }
        "help" => usage(),
        _ => serve(&server, &addr),
    }
}

fn usage() {
    println!("ukma-server — UKMA HTTP surface (spec 1.2.3)");
    println!();
    println!("  ukma-server [serve] [--addr 127.0.0.1:8080]  serve the API");
    println!("  ukma-server --manifest                       print tool-manifest.json");
    println!("  ukma-server --health                         print the health payload");
    println!("  ukma-server --operations                     print operation ids");
}

fn serve(server: &Server, addr: &str) {
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("ukma-server: cannot bind {addr}: {error}");
            std::process::exit(1);
        }
    };
    if let Ok(local) = listener.local_addr() {
        println!("ukma-server listening on http://{local}");
    }
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) = handle_connection(server, stream) {
                    eprintln!("ukma-server: connection error: {error}");
                }
            }
            Err(error) => eprintln!("ukma-server: accept error: {error}"),
        }
    }
}

fn handle_connection(server: &Server, stream: TcpStream) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
    }

    let mut request = HttpRequest::new(&method, &path, &String::from_utf8_lossy(&body));
    request.headers = headers;
    let response = server.handle(request);

    let mut stream = stream;
    let reason = match response.status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        410 => "Gone",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    };
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        response.status,
        response.content_type,
        response.body.len(),
        response.body
    )?;
    stream.flush()
}
