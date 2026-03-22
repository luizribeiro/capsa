use smoltcp::iface::SocketHandle;
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// 1024 simultaneous TCP connections per gateway. Matches typical OS defaults
/// for per-process FD limits and keeps smoltcp's socket set manageable. Each
/// connection holds two JoinHandles + a socket buffer pair, so memory scales
/// linearly (~512 KB per connection at full buffer utilization).
pub(crate) const MAX_TCP_CONNECTIONS: usize = 1024;

/// 256 in-flight connect() calls. Host-side TCP connect is async and can
/// stall for up to TCP_CONNECT_TIMEOUT — capping pending connects prevents
/// a SYN flood from the guest from exhausting tokio task budget while
/// allowing reasonable concurrency for legitimate burst traffic.
pub(crate) const MAX_PENDING_CONNECTS: usize = 256;

/// Standard TCP connect timeout. 10s matches common OS defaults and is long
/// enough for cross-region connections but short enough to reclaim resources
/// from unreachable hosts promptly.
pub(crate) const TCP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Grace period for a connected stream to complete the smoltcp handshake
/// (SYN-ACK → ACK). 30s is generous — the guest-side handshake is over a
/// virtual link with sub-millisecond RTT, so this only triggers on a
/// misbehaving or stalled guest.
pub(crate) const CONNECTED_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// 64 KB host-side read buffer — matches the typical kernel socket receive
/// buffer default and is large enough to fill a smoltcp window in one read,
/// minimizing syscall overhead.
pub(crate) const HOST_READ_BUF: usize = 65536;

/// 1 MB per smoltcp socket direction (send + recv). Sized for one
/// bandwidth-delay product at 1 Gbps with ~10 ms virtual-link RTT
/// (BDP ≈ 1.25 MB). The virtual RTT includes channel traversal,
/// socketpair I/O, KVM virtio processing, and guest kernel scheduling.
/// Per-connection memory: 2 MB (send + recv buffers).
pub(crate) const SMOLTCP_SOCKET_BUF: usize = 1024 * 1024;

/// 4 MB — roughly 4× the smoltcp socket buffer. Only triggers when
/// channel backpressure alone can't keep up (acts as a safety valve).
pub(crate) const UNSENT_PAUSE_THRESHOLD: usize = 4 * 1024 * 1024;

/// 1 MB — resume host reads once the unsent buffer drains below the
/// smoltcp socket buffer size so drain_unsent can make progress.
pub(crate) const UNSENT_RESUME_THRESHOLD: usize = 1024 * 1024;

pub(crate) enum ConnectionState {
    Pending {
        task: JoinHandle<()>,
        created: Instant,
    },
    Connected {
        stream: TcpStream,
        created: Instant,
    },
    Active {
        host_read: JoinHandle<()>,
        host_write: JoinHandle<()>,
        host_write_tx: Option<mpsc::Sender<Vec<u8>>>,
        unsent: VecDeque<u8>,
        read_paused: tokio::sync::watch::Sender<bool>,
    },
}

pub(crate) struct ConnectResult {
    pub handle: SocketHandle,
    pub result: Result<TcpStream, ()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FlowKey {
    pub guest_src: std::net::SocketAddrV4,
    pub remote_dst: std::net::SocketAddrV4,
}

pub(crate) fn abort_connection(state: ConnectionState) {
    match state {
        ConnectionState::Pending { task, .. } => task.abort(),
        ConnectionState::Connected { .. } => {}
        ConnectionState::Active {
            host_read,
            host_write,
            ..
        } => {
            host_read.abort();
            host_write.abort();
        }
    }
}

pub enum InitiateResult {
    Created(SocketHandle),
    DuplicateFlow,
    RejectedLimit,
}
