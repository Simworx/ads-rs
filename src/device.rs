#![allow(missing_docs)]
use itertools::Itertools;
use zerocopy::{AsBytes, FromBytes, LE, U16, U32};

use crate::commands::{
    AddNotif, AddNotifRequest, DelNotifRequest, IndexLength, IndexLengthRW, ReadRequest, ReadState,
    ResultLength, WriteControl, WriteReadRequest, WriteRequest,
};
use crate::errors::Result;
use crate::utils::fixup_write_read_return_buffers;
use crate::{
    commands::{Command, DeviceInfo, DeviceInfoRaw},
    AmsAddr, Client,
};
use crate::{notif, AdsState, Error};
use std::convert::{TryFrom, TryInto};
use std::mem::size_of;

/// A `Client` wrapper that talks to a specific ADS device.
#[derive(Clone, Copy)]
pub struct Device<'c> {
    /// The underlying `Client`.
    pub client: &'c Client,
    ///The address of the device
    pub addr: AmsAddr,
}

impl<'c> Device<'c> {
    /// Read the device's name + version.
    pub fn get_info(&self) -> Result<DeviceInfo> {
        let mut data = DeviceInfoRaw::new_zeroed();
        self.client
            .communicate(Command::DevInfo, self.addr, &[], &mut [data.as_bytes_mut()])?;

        // Decode the name string, which is null-terminated.  Technically it's
        // Windows-1252, but in practice no non-ASCII occurs.
        let name = data
            .name
            .iter()
            .take_while(|&&ch| ch > 0)
            .map(|&ch| ch as char)
            .collect::<String>();
        Ok(DeviceInfo {
            major: data.major,
            minor: data.minor,
            version: data.version.get(),
            name,
        })
    }

    /// Read some data at a given index group/offset.  Returned data can be shorter than
    /// the buffer, the length is the return value.
    pub fn read(&self, index_group: u32, index_offset: u32, data: &mut [u8]) -> Result<usize> {
        let header = IndexLength {
            index_group: U32::new(index_group),
            index_offset: U32::new(index_offset),
            length: U32::new(data.len().try_into()?),
        };
        let mut read_len = U32::<LE>::new(0);

        self.client.communicate(
            Command::Read,
            self.addr,
            &[header.as_bytes()],
            &mut [read_len.as_bytes_mut(), data],
        )?;

        Ok(read_len.get() as usize)
    }

    /// Read some data at a given index group/offset, ensuring that the returned data has
    /// exactly the size of the passed buffer.
    pub fn read_exact(&self, index_group: u32, index_offset: u32, data: &mut [u8]) -> Result<()> {
        let len = self.read(index_group, index_offset, data)?;
        if len != data.len() {
            return Err(Error::Reply(
                "read data",
                "got less data than expected",
                len as u32,
            ));
        }
        Ok(())
    }

    /// Read data of given type.
    ///
    /// Any type that supports `zerocopy::FromBytes` can be read.  You can also
    /// derive that trait on your own structures and read structured data
    /// directly from the symbol.
    ///
    /// Note: to be independent of the host's byte order, use the integer types
    /// defined in `zerocopy::byteorder`.
    pub fn read_value<T: Default + AsBytes + FromBytes>(
        &self,
        index_group: u32,
        index_offset: u32,
    ) -> Result<T> {
        let mut buf = T::default();
        self.read_exact(index_group, index_offset, buf.as_bytes_mut())?;
        Ok(buf)
    }

    /// Read multiple index groups/offsets with one ADS request (a "sum-up" request).
    ///
    /// The returned data can be shorter than the buffer in each case, the `length`
    /// member of the `ReadRequest` is set to the returned length.
    ///
    /// This function only returns Err on errors that cause the whole sum-up
    /// request to fail (e.g. if the device doesn't support such requests).  If
    /// the request as a whole succeeds, each single read can have returned its
    /// own error.  The [`ReadRequest::data`] method will return either the
    /// properly truncated returned data or the error for each read.
    ///
    /// Example:
    /// ```ignore
    /// // create buffers
    /// let mut buf_1 = [0; 128];  // request reading 128 bytes
    /// let mut buf_2 = [0; 128];  // from two indices
    /// // create the request structures
    /// let mut req_1 = ReadRequest::new(ix1, off1, &mut buf_1);
    /// let mut req_2 = ReadRequest::new(ix2, off2, &mut buf_2);
    /// //  actual request
    /// device.read_multi(&mut [req_1, req_2])?;
    /// // extract the resulting data, checking individual reads for
    /// // errors and getting the returned data otherwise
    /// let res_1 = req_1.data()?;
    /// let res_2 = req_2.data()?;
    /// ```
    pub fn read_multi(&self, requests: &mut [ReadRequest]) -> Result<()> {
        let nreq = requests.len();
        let rlen = requests
            .iter()
            .map(|r| size_of::<ResultLength>() + r.rbuf.len())
            .sum::<usize>();
        let wlen = size_of::<IndexLength>() * nreq;
        let header = IndexLengthRW {
            // using SUMUP_READ_EX_2 since would return the actual returned
            // number of bytes, and no empty bytes if the read is short,
            // but then we'd have to reshuffle the buffers
            index_group: U32::new(crate::index::SUMUP_READ_EX),
            index_offset: U32::new(nreq as u32),
            read_length: U32::new(rlen.try_into()?),
            write_length: U32::new(wlen.try_into()?),
        };
        let mut read_len = U32::<LE>::new(0);
        let mut w_buffers = vec![header.as_bytes()];
        let mut r_buffers = (0..2 * nreq + 1).map(|_| &mut [][..]).collect_vec();
        r_buffers[0] = read_len.as_bytes_mut();
        for (i, req) in requests.iter_mut().enumerate() {
            w_buffers.push(req.req.as_bytes());
            r_buffers[1 + i] = req.res.as_bytes_mut();
            r_buffers[1 + nreq + i] = req.rbuf;
        }
        self.client
            .communicate(Command::ReadWrite, self.addr, &w_buffers, &mut r_buffers)?;
        Ok(())
    }

    /// Write some data to a given index group/offset.
    pub fn write(&self, index_group: u32, index_offset: u32, data: &[u8]) -> Result<()> {
        let header = IndexLength {
            index_group: U32::new(index_group),
            index_offset: U32::new(index_offset),
            length: U32::new(data.len().try_into()?),
        };
        self.client.communicate(
            Command::Write,
            self.addr,
            &[header.as_bytes(), data],
            &mut [],
        )?;
        Ok(())
    }

    /// Write data of given type.
    ///
    /// See `read_value` for details.
    pub fn write_value<T: AsBytes>(
        &self,
        index_group: u32,
        index_offset: u32,
        value: &T,
    ) -> Result<()> {
        self.write(index_group, index_offset, value.as_bytes())
    }

    /// Write multiple index groups/offsets with one ADS request (a "sum-up" request).
    ///
    /// This function only returns Err on errors that cause the whole sum-up
    /// request to fail (e.g. if the device doesn't support such requests).  If
    /// the request as a whole succeeds, each single write can have returned its
    /// own error.  The [`WriteRequest::ensure`] method will return the error for
    /// each write.
    pub fn write_multi(&self, requests: &mut [WriteRequest]) -> Result<()> {
        let nreq = requests.len();
        let rlen = size_of::<u32>() * nreq;
        let wlen = requests
            .iter()
            .map(|r| size_of::<IndexLength>() + r.wbuf.len())
            .sum::<usize>();
        let header = IndexLengthRW {
            index_group: U32::new(crate::index::SUMUP_WRITE),
            index_offset: U32::new(nreq as u32),
            read_length: U32::new(rlen.try_into()?),
            write_length: U32::new(wlen.try_into()?),
        };
        let mut read_len = U32::<LE>::new(0);
        let mut w_buffers = vec![&[][..]; 2 * nreq + 1];
        let mut r_buffers = vec![read_len.as_bytes_mut()];
        w_buffers[0] = header.as_bytes();
        for (i, req) in requests.iter_mut().enumerate() {
            w_buffers[1 + i] = req.req.as_bytes();
            w_buffers[1 + nreq + i] = req.wbuf;
            r_buffers.push(req.res.as_bytes_mut());
        }
        self.client
            .communicate(Command::ReadWrite, self.addr, &w_buffers, &mut r_buffers)?;
        Ok(())
    }

    /// Write some data to a given index group/offset and then read back some
    /// reply from there.  This is not the same as a write() followed by read();
    /// it is used as a kind of RPC call.
    pub fn write_read(
        &self,
        index_group: u32,
        index_offset: u32,
        write_data: &[u8],
        read_data: &mut [u8],
    ) -> Result<usize> {
        let header = IndexLengthRW {
            index_group: U32::new(index_group),
            index_offset: U32::new(index_offset),
            read_length: U32::new(read_data.len().try_into()?),
            write_length: U32::new(write_data.len().try_into()?),
        };
        let mut read_len = U32::<LE>::new(0);
        self.client.communicate(
            Command::ReadWrite,
            self.addr,
            &[header.as_bytes(), write_data],
            &mut [read_len.as_bytes_mut(), read_data],
        )?;
        Ok(read_len.get() as usize)
    }

    /// Like `write_read`, but ensure the returned data length matches the output buffer.
    pub fn write_read_exact(
        &self,
        index_group: u32,
        index_offset: u32,
        write_data: &[u8],
        read_data: &mut [u8],
    ) -> Result<()> {
        let len = self.write_read(index_group, index_offset, write_data, read_data)?;
        if len != read_data.len() {
            return Err(Error::Reply(
                "write/read data",
                "got less data than expected",
                len as u32,
            ));
        }
        Ok(())
    }

    /// Write multiple index groups/offsets with one ADS request (a "sum-up" request).
    ///
    /// This function only returns Err on errors that cause the whole sum-up
    /// request to fail (e.g. if the device doesn't support such requests).  If
    /// the request as a whole succeeds, each single write/read can have
    /// returned its own error.  The [`WriteReadRequest::data`] method will
    /// return either the properly truncated returned data or the error for each
    /// write/read.
    pub fn write_read_multi(&self, requests: &mut [WriteReadRequest]) -> Result<()> {
        let nreq = requests.len();
        let rlen = requests
            .iter()
            .map(|r| size_of::<ResultLength>() + r.rbuf.len())
            .sum::<usize>();
        let wlen = requests
            .iter()
            .map(|r| size_of::<IndexLengthRW>() + r.wbuf.len())
            .sum::<usize>();
        let header = IndexLengthRW {
            index_group: U32::new(crate::index::SUMUP_READWRITE),
            index_offset: U32::new(nreq as u32),
            read_length: U32::new(rlen.try_into()?),
            write_length: U32::new(wlen.try_into()?),
        };
        let mut read_len = U32::<LE>::new(0);
        let mut w_buffers = vec![&[][..]; 2 * nreq + 1];
        let mut r_buffers = (0..2 * nreq + 1).map(|_| &mut [][..]).collect_vec();
        w_buffers[0] = header.as_bytes();
        r_buffers[0] = read_len.as_bytes_mut();
        for (i, req) in requests.iter_mut().enumerate() {
            w_buffers[1 + i] = req.req.as_bytes();
            w_buffers[1 + nreq + i] = req.wbuf;
            r_buffers[1 + i] = req.res.as_bytes_mut();
            r_buffers[1 + nreq + i] = req.rbuf;
        }
        self.client
            .communicate(Command::ReadWrite, self.addr, &w_buffers, &mut r_buffers)?;
        // unfortunately SUMUP_READWRITE returns only the actual read bytes for each
        // request, so if there are short reads the buffers got filled wrongly
        fixup_write_read_return_buffers(requests);
        Ok(())
    }

    /// Return the ADS and device state of the device.
    pub fn get_state(&self) -> Result<(AdsState, u16)> {
        let mut state = ReadState::new_zeroed();
        self.client.communicate(
            Command::ReadState,
            self.addr,
            &[],
            &mut [state.as_bytes_mut()],
        )?;

        // Convert ADS state to the enum type
        let ads_state = AdsState::try_from(state.ads_state.get())
            .map_err(|e| Error::Reply("read state", e, state.ads_state.get().into()))?;

        Ok((ads_state, state.dev_state.get()))
    }

    /// (Try to) set the ADS and device state of the device.
    pub fn write_control(&self, ads_state: AdsState, dev_state: u16) -> Result<()> {
        let data = WriteControl {
            ads_state: U16::new(ads_state as _),
            dev_state: U16::new(dev_state),
            data_length: U32::new(0),
        };
        self.client.communicate(
            Command::WriteControl,
            self.addr,
            &[data.as_bytes()],
            &mut [],
        )?;
        Ok(())
    }

    /// Add a notification handle for some index group/offset.
    ///
    /// Notifications are delivered via a MPMC channel whose reading end can be
    /// obtained from `get_notification_channel` on the `Client` object.
    /// The returned `Handle` can be used to check which notification has fired.
    ///
    /// If the notification is not deleted explictly using `delete_notification`
    /// and the `Handle`, it is deleted when the `Client` object is dropped.
    pub fn add_notification(
        &self,
        index_group: u32,
        index_offset: u32,
        attributes: &notif::Attributes,
    ) -> Result<notif::Handle> {
        let data = AddNotif {
            index_group: U32::new(index_group),
            index_offset: U32::new(index_offset),
            length: U32::new(attributes.length.try_into()?),
            trans_mode: U32::new(attributes.trans_mode as u32),
            max_delay: U32::new(attributes.max_delay.as_millis().try_into()?),
            cycle_time: U32::new(attributes.cycle_time.as_millis().try_into()?),
            reserved: [0; 16],
        };
        let mut handle = U32::<LE>::new(0);
        self.client.communicate(
            Command::AddNotification,
            self.addr,
            &[data.as_bytes()],
            &mut [handle.as_bytes_mut()],
        )?;
        self.client
            .notif_handles
            .borrow_mut()
            .insert((self.addr, handle.get()));
        Ok(handle.get())
    }

    /// Add multiple notification handles.
    ///
    /// This function only returns Err on errors that cause the whole sum-up
    /// request to fail (e.g. if the device doesn't support such requests).  If
    /// the request as a whole succeeds, each single read can have returned its
    /// own error.  The [`AddNotifRequest::handle`] method will return either
    /// the returned handle or the error for each read.
    pub fn add_notification_multi(&self, requests: &mut [AddNotifRequest]) -> Result<()> {
        let nreq = requests.len();
        let rlen = size_of::<ResultLength>() * nreq;
        let wlen = size_of::<AddNotif>() * nreq;
        let header = IndexLengthRW {
            index_group: U32::new(crate::index::SUMUP_ADDDEVNOTE),
            index_offset: U32::new(nreq as u32),
            read_length: U32::new(rlen.try_into()?),
            write_length: U32::new(wlen.try_into()?),
        };
        let mut read_len = U32::<LE>::new(0);
        let mut w_buffers = vec![header.as_bytes()];
        let mut r_buffers = vec![read_len.as_bytes_mut()];
        for req in requests.iter_mut() {
            w_buffers.push(req.req.as_bytes());
            r_buffers.push(req.res.as_bytes_mut());
        }
        self.client
            .communicate(Command::ReadWrite, self.addr, &w_buffers, &mut r_buffers)?;
        for req in requests {
            if let Ok(handle) = req.handle() {
                self.client
                    .notif_handles
                    .borrow_mut()
                    .insert((self.addr, handle));
            }
        }
        Ok(())
    }

    /// Delete a notification with given handle.
    pub fn delete_notification(&self, handle: notif::Handle) -> Result<()> {
        self.client.communicate(
            Command::DeleteNotification,
            self.addr,
            &[U32::<LE>::new(handle).as_bytes()],
            &mut [],
        )?;
        self.client
            .notif_handles
            .borrow_mut()
            .remove(&(self.addr, handle));
        Ok(())
    }

    /// Delete multiple notification handles.
    ///
    /// This function only returns Err on errors that cause the whole sum-up
    /// request to fail (e.g. if the device doesn't support such requests).  If
    /// the request as a whole succeeds, each single read can have returned its
    /// own error.  The [`DelNotifRequest::ensure`] method will return either the
    /// returned data or the error for each read.
    pub fn delete_notification_multi(&self, requests: &mut [DelNotifRequest]) -> Result<()> {
        let nreq = requests.len();
        let rlen = size_of::<u32>() * nreq;
        let wlen = size_of::<u32>() * nreq;
        let header = IndexLengthRW {
            index_group: U32::new(crate::index::SUMUP_DELDEVNOTE),
            index_offset: U32::new(nreq as u32),
            read_length: U32::new(rlen.try_into()?),
            write_length: U32::new(wlen.try_into()?),
        };
        let mut read_len = U32::<LE>::new(0);
        let mut w_buffers = vec![header.as_bytes()];
        let mut r_buffers = vec![read_len.as_bytes_mut()];
        for req in requests.iter_mut() {
            w_buffers.push(req.req.as_bytes());
            r_buffers.push(req.res.as_bytes_mut());
        }
        self.client
            .communicate(Command::ReadWrite, self.addr, &w_buffers, &mut r_buffers)?;
        for req in requests {
            if req.ensure().is_ok() {
                self.client
                    .notif_handles
                    .borrow_mut()
                    .remove(&(self.addr, req.req.get()));
            }
        }
        Ok(())
    }
}
