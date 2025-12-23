use nix::sys::socket::{
    connect, socket, AddressFamily, SockFlag, SockType, VsockAddr,
};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;

const HOST_CID: u32 = 2;
const BUFFER_SIZE: usize = 256;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: {} <port>", args[0]);
        std::process::exit(1);
    }

    let port: u32 = match args[1].parse() {
        Ok(p) if p > 0 && p <= 65535 => p,
        _ => {
            eprintln!("Invalid port: {}", args[1]);
            std::process::exit(1);
        }
    };

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
