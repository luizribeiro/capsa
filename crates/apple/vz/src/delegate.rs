//! VM state delegate for receiving lifecycle events from the Virtualization framework.

use objc2::rc::Retained;
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol};
use objc2_virtualization::{VZVirtualMachine, VZVirtualMachineDelegate};
use std::cell::Cell;

pub type StopSender = std::sync::mpsc::SyncSender<VmStopReason>;
pub type StopReceiver = std::sync::mpsc::Receiver<VmStopReason>;

#[derive(Debug, Clone)]
pub enum VmStopReason {
    GuestStopped,
    Error(String),
}

pub struct VmStateDelegateIvars {
    stop_sender: Cell<Option<StopSender>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements
    // - We don't implement Drop
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = VmStateDelegateIvars]
    pub struct VmStateDelegate;

    unsafe impl NSObjectProtocol for VmStateDelegate {}

    unsafe impl VZVirtualMachineDelegate for VmStateDelegate {
        #[unsafe(method(guestDidStopVirtualMachine:))]
        fn guest_did_stop(&self, _vm: &VZVirtualMachine) {
            let sender: Option<StopSender> = self.ivars().stop_sender.take();
            if let Some(sender) = sender {
                let _ = sender.try_send(VmStopReason::GuestStopped);
            }
        }

        #[unsafe(method(virtualMachine:didStopWithError:))]
        fn vm_did_stop_with_error(&self, _vm: &VZVirtualMachine, error: &NSError) {
            let sender: Option<StopSender> = self.ivars().stop_sender.take();
            if let Some(sender) = sender {
                let error_msg = error.localizedDescription().to_string();
                let _ = sender.try_send(VmStopReason::Error(error_msg));
            }
        }
    }
);

impl VmStateDelegate {
    pub fn new(mtm: MainThreadMarker, stop_sender: StopSender) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let this = this.set_ivars(VmStateDelegateIvars {
            stop_sender: Cell::new(Some(stop_sender)),
        });
        // SAFETY: Calling init on a freshly allocated NSObject subclass
        unsafe { objc2::msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod vm_stop_channel {
        use super::*;

        #[test]
        fn channel_receives_guest_stopped() {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            tx.try_send(VmStopReason::GuestStopped).unwrap();

            let result = rx.recv().unwrap();
            assert!(matches!(result, VmStopReason::GuestStopped));
        }

        #[test]
        fn channel_receives_error_with_message() {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            tx.try_send(VmStopReason::Error("test error".to_string()))
                .unwrap();

            let result = rx.recv().unwrap();
            match result {
                VmStopReason::Error(msg) => assert_eq!(msg, "test error"),
                _ => panic!("Expected Error variant"),
            }
        }

        #[test]
        fn channel_disconnection_detected() {
            let (tx, rx) = std::sync::mpsc::sync_channel::<VmStopReason>(1);
            drop(tx);

            let result = rx.recv();
            assert!(result.is_err());
        }

        #[test]
        fn try_send_on_full_channel_fails() {
            let (tx, _rx) = std::sync::mpsc::sync_channel(1);
            tx.try_send(VmStopReason::GuestStopped).unwrap();

            let result = tx.try_send(VmStopReason::GuestStopped);
            assert!(result.is_err());
        }
    }

    mod vm_stop_reason {
        use super::*;

        #[test]
        fn guest_stopped_is_cloneable() {
            let reason = VmStopReason::GuestStopped;
            let cloned = reason.clone();
            assert!(matches!(cloned, VmStopReason::GuestStopped));
        }

        #[test]
        fn error_preserves_message() {
            let reason = VmStopReason::Error("something went wrong".to_string());
            let cloned = reason.clone();
            match cloned {
                VmStopReason::Error(msg) => assert_eq!(msg, "something went wrong"),
                _ => panic!("Expected Error variant"),
            }
        }

        #[test]
        fn debug_format_works() {
            let guest = VmStopReason::GuestStopped;
            let error = VmStopReason::Error("test".to_string());

            assert_eq!(format!("{:?}", guest), "GuestStopped");
            assert_eq!(format!("{:?}", error), "Error(\"test\")");
        }
    }
}
