use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex};
use vm_device::MutDevicePio;
use vm_device::bus::{PioAddress, PioAddressOffset};
use vm_superio::serial::NoEvents;
use vm_superio::{Serial, Trigger};
use vmm_sys_util::eventfd::EventFd;

/// Trigger for vm-superio that signals when an interrupt should be delivered.
pub struct EventFdTrigger {
    fd: Arc<EventFd>,
}

impl EventFdTrigger {
    fn new() -> Self {
        Self {
            fd: Arc::new(EventFd::new(0).expect("failed to create eventfd")),
        }
    }

    fn shared_fd(&self) -> Arc<EventFd> {
        self.fd.clone()
    }
}

impl Trigger for EventFdTrigger {
    type E = std::io::Error;

    fn trigger(&self) -> Result<(), Self::E> {
        self.fd.write(1).map(|_| ())
    }
}

pub struct SerialDevice {
    serial: Mutex<Serial<EventFdTrigger, NoEvents, Box<dyn Write + Send>>>,
    interrupt_evt: Arc<EventFd>,
}

impl SerialDevice {
    pub fn new(output: Box<dyn Write + Send>) -> Self {
        let trigger = EventFdTrigger::new();
        let interrupt_evt = trigger.shared_fd();
        let serial = Serial::with_events(trigger, NoEvents, output);
        Self {
            serial: Mutex::new(serial),
            interrupt_evt,
        }
    }

    pub fn interrupt_evt(&self) -> &EventFd {
        &self.interrupt_evt
    }

    pub fn enqueue_input(&self, data: &[u8]) {
        let mut serial = self.serial.lock().unwrap();
        let _ = serial.enqueue_raw_bytes(data);
    }
}

impl MutDevicePio for SerialDevice {
    fn pio_read(&mut self, _base: PioAddress, offset: PioAddressOffset, data: &mut [u8]) {
        let mut serial = self.serial.lock().unwrap();
        if !data.is_empty() {
            data[0] = serial.read(offset as u8);
        }
    }

    fn pio_write(&mut self, _base: PioAddress, offset: PioAddressOffset, data: &[u8]) {
        if !data.is_empty() {
            let mut serial = self.serial.lock().unwrap();
            serial.write(offset as u8, data[0]).ok();
        }
    }
}

pub fn create_console_pipes() -> io::Result<(OwnedFd, OwnedFd, OwnedFd, OwnedFd)> {
    let (guest_read, host_write) = nix::unistd::pipe()?;
    let (host_read, guest_write) = nix::unistd::pipe()?;
    Ok((guest_read, host_write, host_read, guest_write))
}
