//! rustcraft-bridge: lets RustCraft, the Minecraft client running in a web
//! browser, connect to Minecraft servers. A web page cannot open TCP
//! connections; this program, running on the player's computer, accepts
//! WebSocket connections from the RustCraft page on 127.0.0.1 only and joins
//! each one to a TCP connection to the server the player chose.
//!
//! - `GET /status`: `{"name":"rustcraft-bridge","version":"…"}`, so that the
//!   page can tell whether the bridge runs.
//! - `GET /connect?host=<name or address>&port=<port>` (WebSocket upgrade):
//!   the TCP connection, as binary messages both ways. A name with port 25565
//!   is looked up as Minecraft does: the `_minecraft._tcp` SRV record first.
//!
//! Only pages from the allowed origins may use it (the browser's Origin header;
//! `--allow-origin` adds more), and it listens on the loopback interface only.

mod dns;
mod sha1;
mod ws;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PORT: u16 = 25580;
const DEFAULT_ORIGINS: &[&str] = &[
    "https://rustcraft.online",
    "http://localhost:5173",
    "http://127.0.0.1:5173",
];
/// Connections at once (each is two threads).
const MAX_CONNECTIONS: usize = 32;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

struct Config {
    port: u16,
    origins: Vec<String>,
    verbose: bool,
}

fn main() {
    let config = Arc::new(parse_args());
    let address = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = match TcpListener::bind(address) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("rustcraft-bridge: cannot listen on {address}: {e}");
            std::process::exit(1);
        }
    };
    println!("rustcraft-bridge {VERSION}: listening on ws://{address} for {}", config.origins.join(", "));
    println!("Keep this window open while you play on servers in RustCraft.");
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        let (config, active) = (config.clone(), active.clone());
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, &config, &active) {
                if config.verbose {
                    eprintln!("connection: {e}");
                }
            }
        });
    }
}

fn parse_args() -> Config {
    let mut config = Config { port: DEFAULT_PORT, origins: DEFAULT_ORIGINS.iter().map(|s| s.to_string()).collect(), verbose: false };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => config.port = args.next().and_then(|p| p.parse().ok()).unwrap_or_else(|| usage()),
            "--allow-origin" => config.origins.push(args.next().unwrap_or_else(|| usage()).trim_end_matches('/').to_string()),
            "--verbose" | "-v" => config.verbose = true,
            "--version" => {
                println!("rustcraft-bridge {VERSION}");
                std::process::exit(0);
            }
            _ => usage(),
        }
    }
    config
}

fn usage() -> ! {
    eprintln!("usage: rustcraft-bridge [--port <port>] [--allow-origin <origin>]... [--verbose]");
    std::process::exit(2);
}

/// One HTTP request on `stream`: the status, or a WebSocket bridged to TCP.
fn handle(mut stream: TcpStream, config: &Config, active: &AtomicUsize) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim_end().is_empty() {
            break;
        }
        if let Some((name, value)) = line.trim_end().split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
        if headers.len() > 64 {
            return respond(&mut stream, 431, "", None);
        }
    }
    let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let origin = header("origin").map(|o| o.trim_end_matches('/').to_string());
    let allowed = origin.as_deref().filter(|o| config.origins.iter().any(|a| a == o));
    let (path, query) = target.split_once('?').unwrap_or((target, ""));

    if method == "OPTIONS" {
        return respond(&mut stream, if allowed.is_some() { 204 } else { 403 }, "", allowed);
    }
    let Some(origin) = allowed else {
        // Only the RustCraft page may use the bridge (not any web page).
        return respond(&mut stream, 403, "origin not allowed\n", None);
    };
    match (method, path) {
        ("GET", "/status") => {
            let body = format!("{{\"name\":\"rustcraft-bridge\",\"version\":\"{VERSION}\"}}");
            respond(&mut stream, 200, &body, Some(origin))
        }
        ("GET", "/connect") => {
            let Some(key) = header("sec-websocket-key") else { return respond(&mut stream, 400, "not a WebSocket\n", None) };
            let param = |name: &str| query.split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')).map(percent_decode);
            let (Some(host), Some(port)) = (param("host"), param("port").and_then(|p| p.parse::<u16>().ok())) else {
                return respond(&mut stream, 400, "host and port required\n", None);
            };
            if host.is_empty() || host.len() > 253 || port == 0 {
                return respond(&mut stream, 400, "bad address\n", None);
            }
            if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
                active.fetch_sub(1, Ordering::SeqCst);
                return respond(&mut stream, 503, "too many connections\n", None);
            }
            let result = bridge(stream, reader, key, &host, port, config);
            active.fetch_sub(1, Ordering::SeqCst);
            result
        }
        _ => respond(&mut stream, 404, "", Some(origin)),
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str, origin: Option<&str>) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        431 => "Request Header Fields Too Large",
        _ => "Service Unavailable",
    };
    let mut response = format!("HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n", body.len());
    if let Some(origin) = origin {
        // CORS (and the page's cross-origin isolation, COEP, needs it for /status).
        response += &format!(
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\nAccess-Control-Allow-Methods: GET\r\nAccess-Control-Allow-Private-Network: true\r\nCross-Origin-Resource-Policy: cross-origin\r\n"
        );
    }
    if body.starts_with('{') {
        response += "Content-Type: application/json\r\n";
    }
    response += "\r\n";
    response += body;
    stream.write_all(response.as_bytes())
}

/// Accepts the WebSocket, connects to the server and copies both ways until
/// either side closes.
fn bridge(mut client: TcpStream, reader: BufReader<TcpStream>, key: &str, host: &str, port: u16, config: &Config) -> std::io::Result<()> {
    let server = match connect(host, port) {
        Ok(server) => server,
        Err(e) => {
            if config.verbose {
                eprintln!("{host}:{port}: {e}");
            }
            // The page sees the WebSocket fail (the game: connection refused).
            return respond(&mut client, 502, "", None);
        }
    };
    if config.verbose {
        eprintln!("connected to {host}:{port} ({})", server.peer_addr().map(|a| a.to_string()).unwrap_or_default());
    }
    let accept = ws::accept_key(key);
    client.write_all(
        format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")
            .as_bytes(),
    )?;
    client.set_read_timeout(None)?;
    server.set_nodelay(true)?;
    let _ = client.set_nodelay(true);

    // Server → page: TCP data as binary messages.
    let (mut server_read, mut client_write) = (server.try_clone()?, client.try_clone()?);
    let downstream = std::thread::spawn(move || {
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match server_read.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws::write_frame(&mut client_write, ws::OP_BINARY, &buffer[..n]).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = ws::write_frame(&mut client_write, ws::OP_CLOSE, &[]);
        let _ = client_write.shutdown(Shutdown::Both);
    });
    // Page → server.
    let mut client_read = reader;
    let mut server_write = server;
    loop {
        match ws::read_frame(&mut client_read) {
            Ok(frame) => match frame.opcode {
                ws::OP_BINARY | ws::OP_TEXT | ws::OP_CONTINUATION => {
                    if server_write.write_all(&frame.payload).is_err() {
                        break;
                    }
                }
                ws::OP_PING => {
                    let _ = ws::write_frame(&mut client, ws::OP_PONG, &frame.payload);
                }
                ws::OP_CLOSE => break,
                _ => {}
            },
            Err(_) => break,
        }
    }
    let _ = server_write.shutdown(Shutdown::Both);
    let _ = downstream.join();
    Ok(())
}

/// Connects to `host`:`port`; a name with port 25565 uses its Minecraft SRV
/// record when it has one.
fn connect(host: &str, port: u16) -> std::io::Result<TcpStream> {
    let is_name = host.parse::<std::net::IpAddr>().is_err();
    let (host, port) = match (is_name && port == 25565).then(|| dns::minecraft_srv(host)).flatten() {
        Some((target, srv_port)) => (target, srv_port),
        None => (host.to_string(), port),
    };
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "no address");
    for address in (host.as_str(), port).to_socket_addrs()? {
        match TcpStream::connect_timeout(&address, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
