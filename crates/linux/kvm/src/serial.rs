use std::collections::VecDeque;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::sync::Mutex;
use vm_device::MutDevicePio;
use vm_device::bus::{PioAddress, PioAddressOffset};
use vm_superio::serial::NoEvents;
use vm_superio::{Serial, Trigger};

pub struct EventFdTrigger {
    fd: vmm_sys_util::eventfd::EventFd,
}

impl EventFdTrigger {
    fn new() -> Self {
        Self {
            fd: vmm_sys_util::eventfd::EventFd::new(0).expect("failed to create eventfd"),
        }
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
    input_buffer: Mutex<VecDeque<u8>>,
}

impl SerialDevice {
    pub fn new(output: Box<dyn Write + Send>) -> Self {
        let trigger = EventFdTrigger::new();
        let serial = Serial::with_events(trigger, NoEvents, output);
        Self {
            serial: Mutex::new(serial),
            input_buffer: Mutex::new(VecDeque::new()),
        }
    }

    pub fn enqueue_input(&self, data: &[u8]) {
        let mut input = self.input_buffer.lock().unwrap();
        input.extend(data);
    }
}

impl MutDevicePio for SerialDevice {
    fn pio_read(&mut self, _base: PioAddress, offset: PioAddressOffset, data: &mut [u8]) {
        let mut serial = self.serial.lock().unwrap();

        let mut input = self.input_buffer.lock().unwrap();
        if !input.is_empty() {
            let bytes: Vec<u8> = input.drain(..).collect();
            drop(input);
            serial.enqueue_raw_bytes(&bytes).ok();
        } else {
            drop(input);
        }

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
