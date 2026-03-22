//! Frame bridge for connecting a SocketPairDevice to a VirtualSwitch port.
//!
//! This module provides bidirectional frame forwarding between a VM's network
//! interface (via socketpair) and a VirtualSwitch port for cluster networking.

use super::SwitchPort;
use crate::util::set_nonblocking;
use nix::sys::socket::{recv, send, MsgFlags};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use tracing::{debug, trace, warn};

const MTU: usize = 1518;

/// Channel capacity for the VM writer queue.
/// Matches switch port capacity to avoid bottlenecks.
const BRIDGE_CHANNEL_CAPACITY: usize = crate::config::channel::SWITCH_PORT;

/// Bridge a VM's network socketpair to a VirtualSwitch port.
///
/// This function runs bidirectionally:
/// - Frames from VM (via socketpair) → VirtualSwitch port
/// - Frames from VirtualSwitch port → VM (via socketpair)
///
/// Returns when either side closes or encounters an error.
pub async fn bridge_to_switch(host_fd: OwnedFd, port: SwitchPort) -> io::Result<()> {
    set_nonblocking(&host_fd)?;
    super::socketpair::increase_socket_buffer(&host_fd)?;
    let async_fd = Arc::new(AsyncFd::new(host_fd)?);

    // Channel from switch to VM writer task
    let (to_vm_tx, mut to_vm_rx) = mpsc::channel::<Vec<u8>>(BRIDGE_CHANNEL_CAPACITY);

    // Get the internal channels from SwitchPort
    let port_sender = port.sender();
    let mut port_receiver = port.into_receiver();

    // Task: Read from VM socketpair, send to switch port
    let mut vm_to_switch = {
        let async_fd = async_fd.clone();
        crate::util::spawn_named("net-vm-to-switch", async move {
            let mut buf = [0u8; MTU];
            loop {
                // Wait for socketpair to be readable
                let len = match async_fd.readable().await {
                    Ok(mut guard) => {
                        match guard.try_io(|inner| {
                            recv(inner.get_ref().as_raw_fd(), &mut buf, MsgFlags::empty())
                                .map_err(|e| io::Error::from_raw_os_error(e as i32))
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
    let mut switch_to_vm = crate::util::spawn_named("net-switch-to-vm", async move {
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
    let mut vm_writer = {
        let async_fd = async_fd.clone();
        crate::util::spawn_named("net-vm-writer", async move {
            while let Some(frame) = to_vm_rx.recv().await {
                // Wait for socketpair to be writable and send
                loop {
                    match async_fd.writable().await {
                        Ok(mut guard) => {
                            match guard.try_io(|inner| {
                                let n =
                                    send(inner.get_ref().as_raw_fd(), &frame, MsgFlags::empty())
                                        .map_err(|e| io::Error::from_raw_os_error(e as i32))?;
                                if n != frame.len() {
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

    // Wait for any task to complete, then stop the remaining tasks.
    tokio::select! {
        _ = &mut vm_to_switch => debug!("bridge: VM→switch task completed"),
        _ = &mut switch_to_vm => debug!("bridge: switch→VM task completed"),
        _ = &mut vm_writer => debug!("bridge: VM writer task completed"),
    }

    vm_to_switch.abort();
    switch_to_vm.abort();
    vm_writer.abort();

    let _ = vm_to_switch.await;
    let _ = switch_to_vm.await;
    let _ = vm_writer.await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::socket::{socketpair, AddressFamily, SockFlag, SockType};

    fn sample_frame(src_mac: [u8; 6], dst_mac: [u8; 6]) -> Vec<u8> {
        let mut frame = vec![0u8; 64];
        frame[0..6].copy_from_slice(&dst_mac);
        frame[6..12].copy_from_slice(&src_mac);
        frame[12..14].copy_from_slice(&[0x08, 0x00]);
        frame
    }

    #[tokio::test]
    async fn bridge_forwards_vm_frames_to_switch() {
        let switch = crate::VirtualSwitch::new();
        let bridged_port = switch.create_port().await;
        let mut observer_port = switch.create_port().await;

        let (host_fd, guest_fd) = socketpair(
            AddressFamily::Unix,
            SockType::Datagram,
            None,
            SockFlag::empty(),
        )
        .expect("socketpair failed");

        let bridge = tokio::spawn(async move { bridge_to_switch(host_fd, bridged_port).await });

        let frame = sample_frame(
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x01],
            [0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        );
        send(guest_fd.as_raw_fd(), &frame, MsgFlags::empty())
            .expect("send guest->host frame failed");

        let received = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            observer_port.recv_frame(),
        )
        .await
        .expect("expected frame on observer port")
        .expect("observer port receive failed");
        assert_eq!(received, frame);

        drop(guest_fd);
        bridge.abort();
        let _ = bridge.await;
    }

    #[tokio::test]
    async fn bridge_forwards_switch_frames_to_vm() {
        let switch = crate::VirtualSwitch::new();
        let bridged_port = switch.create_port().await;
        let source_port = switch.create_port().await;

        let (host_fd, guest_fd) = socketpair(
            AddressFamily::Unix,
            SockType::Datagram,
            None,
            SockFlag::empty(),
        )
        .expect("socketpair failed");

        let bridge = tokio::spawn(async move { bridge_to_switch(host_fd, bridged_port).await });

        let frame = sample_frame(
            [0x02, 0x00, 0x00, 0x00, 0x00, 0x02],
            [0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        );
        source_port
            .sender()
            .send(frame.clone())
            .await
            .expect("switch send failed");

        let mut buf = [0u8; MTU];
        let len = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            loop {
                match recv(guest_fd.as_raw_fd(), &mut buf, MsgFlags::MSG_DONTWAIT) {
                    Ok(n) => return n,
                    Err(nix::errno::Errno::EAGAIN) => tokio::task::yield_now().await,
                    Err(e) => panic!("recv failed: {e}"),
                }
            }
        })
        .await
        .expect("expected frame on guest fd");

        assert_eq!(&buf[..len], &frame);

        drop(guest_fd);
        bridge.abort();
        let _ = bridge.await;
    }
}
