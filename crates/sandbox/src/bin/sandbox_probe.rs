use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(action) = args.next() else {
        usage_and_exit();
    };

    let result = match action.as_str() {
        "can-read" => {
            let Some(path) = args.next() else {
                usage_and_exit();
            };
            can_read(Path::new(&path))
        }
        "can-write" => {
            let Some(path) = args.next() else {
                usage_and_exit();
            };
            can_write(Path::new(&path))
        }
        "can-stat" => {
            let Some(path) = args.next() else {
                usage_and_exit();
            };
            can_stat(Path::new(&path))
        }
        "can-exec" => {
            let Some(path) = args.next() else {
                usage_and_exit();
            };
            let rest: Vec<String> = args.collect();
            can_exec(Path::new(&path), &rest)
        }
        "can-connect" => {
            let Some(host) = args.next() else {
                usage_and_exit();
            };
            let Some(port) = args.next() else {
                usage_and_exit();
            };
            can_connect(&host, &port)
        }
        "can-write-temp" => can_write_temp(),
        _ => {
            usage_and_exit();
        }
    };

    if let Err(err) = result {
        eprintln!("sandbox-probe action `{action}` failed: {err}");
        std::process::exit(1);
    }
}

fn can_read(path: &Path) -> Result<(), String> {
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(())
}

fn can_write(path: &Path) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(b"capsa-sandbox-probe\n")
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn can_stat(path: &Path) -> Result<(), String> {
    std::fs::metadata(path).map_err(|e| e.to_string())?;
    Ok(())
}

fn can_exec(path: &Path, args: &[String]) -> Result<(), String> {
    let status = Command::new(path)
        .args(args)
        .status()
        .map_err(|e| e.to_string())?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("command exited with status {status}"))
    }
}

fn can_connect(host: &str, port: &str) -> Result<(), String> {
    let port: u16 = port
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
    Ok(())
}

fn can_write_temp() -> Result<(), String> {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "capsa-sandbox-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    file.write_all(b"capsa-sandbox-probe-temp\n")
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn usage_and_exit() -> ! {
    eprintln!(
        "usage: sandbox_probe <action> [args...]\n\
actions:\n\
  can-read <path>\n\
  can-write <path>\n\
  can-stat <path>\n\
  can-exec <path> [args...]\n\
  can-connect <host> <port>\n\
  can-write-temp"
    );
    std::process::exit(2);
}
