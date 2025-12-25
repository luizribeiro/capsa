//! Frame bridge for connecting a SocketPairDevice to a VirtualSwitch port.
//!
//! This module provides bidirectional frame forwarding between a VM's network
//! interface (via socketpair) and a VirtualSwitch port for cluster networking.

use crate::SwitchPort;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

const MTU: usize = 1518;

/// Bridge a VM's network socketpair to a VirtualSwitch port.
///
/// This function runs bidirectionally:
/// - Frames from VM (via socketpair) → VirtualSwitch port
/// - Frames from VirtualSwitch port → VM (via socketpair)
///
/// Returns when either side closes or encounters an error.
pub async fn bridge_to_switch(host_fd: OwnedFd, port: SwitchPort) -> io::Result<()> {
    set_nonblocking(&host_fd)?;
    let async_fd = Arc::new(AsyncFd::new(host_fd)?);

    // Channel from switch to VM writer task
    let (to_vm_tx, mut to_vm_rx) = mpsc::channel::<Vec<u8>>(256);

    // Get the internal channels from SwitchPort
    let port_sender = port.sender();
    let mut port_receiver = port.into_receiver();

    // Task: Read from VM socketpair, send to switch port
    let vm_to_switch = {
        let async_fd = async_fd.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; MTU];
            loop {
                // Wait for socketpair to be readable
                let len = match async_fd.readable().await {
                    Ok(mut guard) => {
                        match guard.try_io(|inner| {
                            let fd = inner.as_raw_fd();
                            let n =
                                unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0) };
                            if n < 0 {
                                Err(io::Error::last_os_error())
                            } else {
                                Ok(n as usize)
                            }
                        }) {
                            Ok(Ok(len)) => len,
                            Ok(Err(e)) => {
                                warn!(error = %e, "bridge: error reading from VM");
                                break;
                            }
                            Err(_would_block) => continue,
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "bridge: error waiting for VM readable");
                        break;
                    }
                };

                if len == 0 {
                    debug!("bridge: VM socketpair closed");
                    break;
                }

                trace!(len, "bridge: forwarding frame VM → switch");
                if port_sender.send(buf[..len].to_vec()).await.is_err() {
                    debug!("bridge: switch port closed");
                    break;
                }
            }
        })
    };

    // Task: Read from switch port, send to VM socketpair channel
    let switch_to_vm = tokio::spawn(async move {
        while let Some(frame) = port_receiver.recv().await {
            trace!(len = frame.len(), "bridge: forwarding frame switch → VM");
            if to_vm_tx.send(frame).await.is_err() {
                debug!("bridge: VM channel closed");
                break;
            }
        }
        debug!("bridge: switch port receiver closed");
    });

    // Task: Write frames to VM socketpair
    let vm_writer = {
        let async_fd = async_fd.clone();
        tokio::spawn(async move {
            while let Some(frame) = to_vm_rx.recv().await {
                // Wait for socketpair to be writable and send
                loop {
                    match async_fd.writable().await {
                        Ok(mut guard) => {
                            match guard.try_io(|inner| {
                                let fd = inner.as_raw_fd();
                                let n = unsafe {
                                    libc::send(fd, frame.as_ptr() as *const _, frame.len(), 0)
                                };
                                if n < 0 {
                                    Err(io::Error::last_os_error())
                                } else if n as usize != frame.len() {
                                    Err(io::Error::new(
                                        io::ErrorKind::WriteZero,
                                        "incomplete frame send",
                                    ))
                                } else {
                                    Ok(())
                                }
                            }) {
                                Ok(Ok(())) => break,
                                Ok(Err(e)) => {
                                    warn!(error = %e, "bridge: error writing to VM");
                                    return;
                                }
                                Err(_would_block) => continue,
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "bridge: error waiting for VM writable");
                            return;
                        }
                    }
                }
            }
        })
    };

    // Wait for any task to complete
    tokio::select! {
        _ = vm_to_switch => debug!("bridge: VM→switch task completed"),
        _ = switch_to_vm => debug!("bridge: switch→VM task completed"),
        _ = vm_writer => debug!("bridge: VM writer task completed"),
    }

    Ok(())
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
