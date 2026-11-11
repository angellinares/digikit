//! The real link: the vendor interface of the device (class 0xFF) on USB bulk endpoints, through nusb.
//!
//! Two units of the same model have the same USB serial strings, so a device is named by where it is on the bus (`bus:address`, `Candidate::id`), and
//! the caller proves which candidate is the one on its MIDI port with the nonce handshake (docs/plans/usb-fast.md section 5).
use std::time::Duration;

use nusb::transfer::{Bulk, Direction, In, Out, TransferError};
use nusb::{DeviceInfo, Endpoint, Interface, MaybeFuture};

use crate::gen_bulk::{CLASS, VID};
use crate::link::{FrameReader, InPipe, Link, LinkError, OutPipe, send_with_zlp};

/// Size of one IN transfer: replies are small; a multiple of every bulk packet size (64, 512).
const IN_SIZE: usize = 4096;

/// A device that offers the bulk interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// `bus:address`: valid while the device stays plugged in.
    pub id: String,
    /// The product string, for the log.
    pub name: String,
    pub interface: u8,
}

fn id_of(d: &DeviceInfo) -> String {
    format!("{}:{}", d.bus_id(), d.device_address())
}

fn offers(d: &DeviceInfo) -> Option<u8> {
    (d.vendor_id() == VID)
        .then(|| {
            d.interfaces()
                .find(|i| i.class() == CLASS)
                .map(|i| i.interface_number())
        })
        .flatten()
}

/// The devices with the vendor id and an interface of the bulk class.
pub fn list() -> Result<Vec<Candidate>, String> {
    let all = nusb::list_devices().wait().map_err(|e| e.to_string())?;
    Ok(all
        .filter_map(|d| {
            let interface = offers(&d)?;
            Some(Candidate {
                id: id_of(&d),
                name: d.product_string().unwrap_or("?").to_string(),
                interface,
            })
        })
        .collect())
}

/// The OUT half.
pub struct UsbTx {
    out: Endpoint<Bulk, Out>,
}
/// The IN half; `recv` returns whole frames; the device sends 64-byte IN packets (each a short packet, no zero-length packets), so it reads until a frame is complete.
pub struct UsbRx {
    inp: Endpoint<Bulk, In>,
    reader: FrameReader,
}
pub struct UsbLink {
    pub tx: UsbTx,
    pub rx: UsbRx,
    interface: Interface,
}

impl UsbLink {
    /// Open the device `id` (`bus:address` of `list`), or the first candidate when `id` is None, claim its bulk interface and take its endpoints.
    pub fn open(id: Option<&str>) -> Result<UsbLink, String> {
        let all = nusb::list_devices().wait().map_err(|e| e.to_string())?;
        let info = all
            .filter(|d| offers(d).is_some())
            .find(|d| id.is_none_or(|w| id_of(d) == w))
            .ok_or_else(|| match id {
                Some(w) => format!("no USB device {w} with an interface of class {CLASS:#04x}"),
                None => format!("no USB device {VID:#06x} with an interface of class {CLASS:#04x} (is usb-fast loaded?)"),
            })?;
        let number = offers(&info).unwrap();
        let device = info.open().wait().map_err(|e| format!("open: {e}"))?;
        let interface = device
            .claim_interface(number)
            .wait()
            .map_err(|e| format!("claim interface {number}: {e}"))?;
        let (mut out_addr, mut in_addr) = (None, None);
        for ep in interface
            .descriptor()
            .into_iter()
            .flat_map(|d| d.endpoints())
        {
            if ep.transfer_type() != nusb::descriptors::TransferType::Bulk {
                continue;
            }
            match ep.direction() {
                Direction::Out => out_addr = out_addr.or(Some(ep.address())),
                Direction::In => in_addr = in_addr.or(Some(ep.address())),
            }
        }
        let (Some(o), Some(i)) = (out_addr, in_addr) else {
            return Err(format!(
                "interface {number} has no bulk OUT and bulk IN endpoint pair"
            ));
        };
        let out = interface
            .endpoint::<Bulk, Out>(o)
            .map_err(|e| format!("endpoint {o:#04x}: {e}"))?;
        let inp = interface
            .endpoint::<Bulk, In>(i)
            .map_err(|e| format!("endpoint {i:#04x}: {e}"))?;
        Ok(UsbLink {
            tx: UsbTx { out },
            rx: UsbRx {
                inp,
                reader: FrameReader::default(),
            },
            interface,
        })
    }

    /// The two halves, for a bridge that sends and receives on different threads; the interface stays claimed until the third value drops.
    pub fn split(self) -> (UsbTx, UsbRx, Box<dyn std::any::Any + Send>) {
        (self.tx, self.rx, Box::new(self.interface))
    }
}

fn map(e: TransferError) -> LinkError {
    match e {
        TransferError::Disconnected => LinkError::Disconnected,
        TransferError::Cancelled => LinkError::Timeout,
        other => LinkError::Io(other.to_string()),
    }
}

impl OutPipe for UsbTx {
    fn out(&mut self, data: &[u8], timeout: Duration) -> Result<(), LinkError> {
        self.out.submit(data.to_vec().into());
        match self.out.wait_next_complete(timeout) {
            Some(c) => c.status.map_err(|e| {
                if e == TransferError::Stall {
                    let _ = self.out.clear_halt().wait();
                }
                map(e)
            }),
            None => {
                self.out.cancel_all();
                let _ = self.out.wait_next_complete(Duration::from_millis(500));
                Err(LinkError::Timeout)
            }
        }
    }
}

impl UsbTx {
    /// One frame, then a zero-length transfer when its length is a multiple of the packet size.
    pub fn send(&mut self, frame: &[u8], timeout: Duration) -> Result<(), LinkError> {
        send_with_zlp(self, frame, timeout)
    }
}

impl InPipe for UsbRx {
    fn read(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        if self.inp.pending() == 0 {
            let buf = self.inp.allocate(IN_SIZE);
            self.inp.submit(buf);
        }
        // a timeout leaves the transfer pending: the next read waits on it
        let c = self
            .inp
            .wait_next_complete(timeout)
            .ok_or(LinkError::Timeout)?;
        if let Err(e) = c.status {
            if e == TransferError::Stall {
                let _ = self.inp.clear_halt().wait();
            }
            return Err(map(e));
        }
        Ok(c.buffer[..c.actual_len].to_vec())
    }
}

impl UsbRx {
    pub fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        let mut reader = std::mem::take(&mut self.reader);
        let r = reader.next(self, timeout);
        self.reader = reader;
        r
    }
}

impl Link for UsbLink {
    fn send(&mut self, frame: &[u8], timeout: Duration) -> Result<(), LinkError> {
        self.tx.send(frame, timeout)
    }
    fn recv(&mut self, timeout: Duration) -> Result<Vec<u8>, LinkError> {
        self.rx.recv(timeout)
    }
    fn close(&mut self) {
        self.tx.out.cancel_all();
        self.rx.inp.cancel_all();
    }
}

impl Drop for UsbTx {
    fn drop(&mut self) {
        self.out.cancel_all();
    }
}
impl Drop for UsbRx {
    fn drop(&mut self) {
        self.inp.cancel_all();
    }
}
