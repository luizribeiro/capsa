use nix::sys::socket::{
    accept, bind, connect, listen, socket, AddressFamily, Backlog, SockFlag, SockType, VsockAddr,
};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

const HOST_CID: u32 = 2;
const VMADDR_CID_ANY: u32 = u32::MAX;
const BUFFER_SIZE: usize = 256;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("Usage: {} <connect|listen> <port>", args[0]);
        std::process::exit(1);
    }

    let mode = &args[1];
    let port: u32 = match args[2].parse() {
        Ok(p) if p > 0 && p <= 65535 => p,
        _ => {
            eprintln!("Invalid port: {}", args[2]);
            std::process::exit(1);
        }
    };

    match mode.as_str() {
        "connect" => run_connect(port),
        "listen" => run_listen(port),
        _ => {
            eprintln!("Unknown mode: {}. Use 'connect' or 'listen'", mode);
            std::process::exit(1);
        }
    }
}

fn run_connect(port: u32) {
    println!("vsock-pong: connecting to host CID {} port {}", HOST_CID, port);

    let fd = socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .expect("socket");

    let addr = VsockAddr::new(HOST_CID, port);
    connect(fd.as_raw_fd(), &addr).expect("connect");

    println!("vsock-pong: connected!");

    handle_connection(fd);
}

fn run_listen(port: u32) {
    println!("vsock-pong: listening on port {}", port);

    let fd = socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .expect("socket");

    let addr = VsockAddr::new(VMADDR_CID_ANY, port);
    bind(fd.as_raw_fd(), &addr).expect("bind");
    listen(&fd, Backlog::new(1).unwrap()).expect("listen");

    println!("vsock-pong: waiting for connection...");

    let conn_fd = accept(fd.as_raw_fd()).expect("accept");
    let conn_fd = unsafe { OwnedFd::from_raw_fd(conn_fd) };

    println!("vsock-pong: accepted connection!");

    handle_connection(conn_fd);
}

fn handle_connection(fd: OwnedFd) {
    let mut stream = File::from(fd);
    let mut buffer = [0u8; BUFFER_SIZE];

    loop {
        let n = match stream.read(&mut buffer[..BUFFER_SIZE - 1]) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                eprintln!("read: {}", e);
                break;
            }
        };

        let mut end = n;
        while end > 0 && (buffer[end - 1] == b'\n' || buffer[end - 1] == b'\r') {
            end -= 1;
        }
        let msg = std::str::from_utf8(&buffer[..end]).unwrap_or("");

        println!("vsock-pong: received '{}'", msg);

        if msg == "ping" {
            if let Err(e) = stream.write_all(b"pong") {
                eprintln!("write: {}", e);
                break;
            }
            println!("vsock-pong: sent 'pong'");
        } else if msg == "quit" {
            println!("vsock-pong: received quit, exiting");
            break;
        } else if !msg.is_empty() {
            if let Err(e) = stream.write_all(&buffer[..end]) {
                eprintln!("write: {}", e);
                break;
            }
        }
    }

    println!("vsock-pong: connection closed");
}
