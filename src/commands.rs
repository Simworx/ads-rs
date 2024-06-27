use crate::errors::ads_error;
use crate::notif;
use crate::{AmsNetId, Result};

use zerocopy::byteorder::{U16, U32};
use zerocopy::{AsBytes, FromBytes, LE};

/// An ADS protocol command.
// https://infosys.beckhoff.com/content/1033/tc3_ads_intro/115847307.html?id=7738940192708835096
#[repr(u16)]
#[derive(Clone, Copy, Debug)]
pub enum Command {
    /// Return device info
    DevInfo = 1,
    /// Read some data
    Read = 2,
    /// Write some data
    Write = 3,
    /// Write some data, then read back some data
    /// (used as a poor-man's function call)
    ReadWrite = 9,
    /// Read the ADS and device state
    ReadState = 4,
    /// Set the ADS and device state
    WriteControl = 5,
    /// Add a notification for a given index
    AddNotification = 6,
    /// Add a notification for a given index
    DeleteNotification = 7,
    /// Change occurred in a given notification,
    /// can be sent by the PLC only
    Notification = 8,
}

/// Device info returned from an ADS server.
#[derive(Debug)]
pub struct DeviceInfo {
    /// Name of the ADS device/service.
    pub name: String,
    /// Major version.
    pub major: u8,
    /// Minor version.
    pub minor: u8,
    /// Build version.
    pub version: u16,
}

impl Command {
    pub fn action(self) -> &'static str {
        match self {
            Command::DevInfo => "get device info",
            Command::Read => "read data",
            Command::Write => "write data",
            Command::ReadWrite => "write and read data",
            Command::ReadState => "read state",
            Command::WriteControl => "write control",
            Command::AddNotification => "add notification",
            Command::DeleteNotification => "delete notification",
            Command::Notification => "notification",
        }
    }
}

// Structures used in communication, not exposed to user,
// but pub(crate) for the test suite.

#[derive(AsBytes, FromBytes, Debug)]
#[repr(C)]
pub(crate) struct AdsHeader {
    /// 0x0 - ADS command
    /// 0x1 - close port
    /// 0x1000 - open port
    /// 0x1001 - note from router (router state changed)
    /// 0x1002 - get local netid
    pub ams_cmd: u16,
    pub length: U32<LE>,
    pub dest_netid: AmsNetId,
    pub dest_port: U16<LE>,
    pub src_netid: AmsNetId,
    pub src_port: U16<LE>,
    pub command: U16<LE>,
    /// 0x01 - response
    /// 0x02 - no return
    /// 0x04 - ADS command
    /// 0x08 - system command
    /// 0x10 - high priority
    /// 0x20 - with time stamp (8 bytes added)
    /// 0x40 - UDP
    /// 0x80 - command during init phase
    /// 0x8000 - broadcast
    pub state_flags: U16<LE>,
    pub data_length: U32<LE>,
    pub error_code: U32<LE>,
    pub invoke_id: U32<LE>,
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub(crate) struct DeviceInfoRaw {
    pub major: u8,
    pub minor: u8,
    pub version: U16<LE>,
    pub name: [u8; 16],
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct IndexLength {
    pub index_group: U32<LE>,
    pub index_offset: U32<LE>,
    pub length: U32<LE>,
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct ResultLength {
    pub result: U32<LE>,
    pub length: U32<LE>,
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct IndexLengthRW {
    pub index_group: U32<LE>,
    pub index_offset: U32<LE>,
    pub read_length: U32<LE>,
    pub write_length: U32<LE>,
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub(crate) struct ReadState {
    pub ads_state: U16<LE>,
    pub dev_state: U16<LE>,
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub(crate) struct WriteControl {
    pub ads_state: U16<LE>,
    pub dev_state: U16<LE>,
    pub data_length: U32<LE>,
}

#[derive(FromBytes, AsBytes)]
#[repr(C)]
pub struct AddNotif {
    pub index_group: U32<LE>,
    pub index_offset: U32<LE>,
    pub length: U32<LE>,
    pub trans_mode: U32<LE>,
    pub max_delay: U32<LE>,
    pub cycle_time: U32<LE>,
    pub reserved: [u8; 16],
}

/// A single request for a [`Device::read_multi`] request.
pub struct ReadRequest<'buf> {
    pub req: IndexLength,
    pub res: ResultLength,
    pub rbuf: &'buf mut [u8],
}

impl<'buf> ReadRequest<'buf> {
    /// Create the request with given index group, index offset and result buffer.
    pub fn new(index_group: u32, index_offset: u32, buffer: &'buf mut [u8]) -> Self {
        Self {
            req: IndexLength {
                index_group: U32::new(index_group),
                index_offset: U32::new(index_offset),
                length: U32::new(buffer.len() as u32),
            },
            res: ResultLength::new_zeroed(),
            rbuf: buffer,
        }
    }

    /// Get the actual returned data.
    ///
    /// If the request returned an error, returns Err.
    pub fn data(&self) -> Result<&[u8]> {
        if self.res.result.get() != 0 {
            ads_error("multi-read data", self.res.result.get())
        } else {
            Ok(&self.rbuf[..self.res.length.get() as usize])
        }
    }
}

/// A single request for a [`Device::write_multi`] request.
pub struct WriteRequest<'buf> {
    pub req: IndexLength,
    pub res: U32<LE>,
    pub wbuf: &'buf [u8],
}

impl<'buf> WriteRequest<'buf> {
    /// Create the request with given index group, index offset and input buffer.
    pub fn new(index_group: u32, index_offset: u32, buffer: &'buf [u8]) -> Self {
        Self {
            req: IndexLength {
                index_group: U32::new(index_group),
                index_offset: U32::new(index_offset),
                length: U32::new(buffer.len() as u32),
            },
            res: U32::default(),
            wbuf: buffer,
        }
    }

    /// Verify that the data was successfully written.
    ///
    /// If the request returned an error, returns Err.
    pub fn ensure(&self) -> Result<()> {
        if self.res.get() != 0 {
            ads_error("multi-write data", self.res.get())
        } else {
            Ok(())
        }
    }
}

/// A single request for a [`Device::write_read_multi`] request.
pub struct WriteReadRequest<'buf> {
    pub req: IndexLengthRW,
    pub res: ResultLength,
    pub wbuf: &'buf [u8],
    pub rbuf: &'buf mut [u8],
}

impl<'buf> WriteReadRequest<'buf> {
    /// Create the request with given index group, index offset and input and
    /// result buffers.
    pub fn new(
        index_group: u32,
        index_offset: u32,
        write_buffer: &'buf [u8],
        read_buffer: &'buf mut [u8],
    ) -> Self {
        Self {
            req: IndexLengthRW {
                index_group: U32::new(index_group),
                index_offset: U32::new(index_offset),
                read_length: U32::new(read_buffer.len() as u32),
                write_length: U32::new(write_buffer.len() as u32),
            },
            res: ResultLength::new_zeroed(),
            wbuf: write_buffer,
            rbuf: read_buffer,
        }
    }

    /// Get the actual returned data.
    ///
    /// If the request returned an error, returns Err.
    pub fn data(&self) -> Result<&[u8]> {
        if self.res.result.get() != 0 {
            ads_error("multi-read/write data", self.res.result.get())
        } else {
            Ok(&self.rbuf[..self.res.length.get() as usize])
        }
    }
}

/// A single request for a [`Device::add_notification_multi`] request.
pub struct AddNotifRequest {
    pub req: AddNotif,
    pub res: ResultLength, // length is the handle
}

impl AddNotifRequest {
    /// Create the request with given index group, index offset and notification
    /// attributes.
    pub fn new(index_group: u32, index_offset: u32, attributes: &notif::Attributes) -> Self {
        Self {
            req: AddNotif {
                index_group: U32::new(index_group),
                index_offset: U32::new(index_offset),
                length: U32::new(attributes.length as u32),
                trans_mode: U32::new(attributes.trans_mode as u32),
                max_delay: U32::new(attributes.max_delay.as_millis() as u32),
                cycle_time: U32::new(attributes.cycle_time.as_millis() as u32),
                reserved: [0; 16],
            },
            res: ResultLength::new_zeroed(),
        }
    }

    /// Get the returned notification handle.
    ///
    /// If the request returned an error, returns Err.
    pub fn handle(&self) -> Result<notif::Handle> {
        if self.res.result.get() != 0 {
            ads_error("multi-read/write data", self.res.result.get())
        } else {
            Ok(self.res.length.get())
        }
    }
}

/// A single request for a [`Device::delete_notification_multi`] request.
pub struct DelNotifRequest {
    pub req: U32<LE>,
    pub res: U32<LE>,
}

impl DelNotifRequest {
    /// Create the request with given index group, index offset and notification
    /// attributes.
    pub fn new(handle: notif::Handle) -> Self {
        Self {
            req: U32::new(handle),
            res: U32::default(),
        }
    }

    /// Verify that the handle was successfully deleted.
    ///
    /// If the request returned an error, returns Err.
    pub fn ensure(&self) -> Result<()> {
        if self.res.get() != 0 {
            ads_error("multi-read/write data", self.res.get())
        } else {
            Ok(())
        }
    }
}
