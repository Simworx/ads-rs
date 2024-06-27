//! Contains the TCP client to connect to an ADS server.
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::convert::TryInto;
use std::io::{self, Read, Write};
use std::net::{IpAddr, Shutdown, TcpStream, ToSocketAddrs};

use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use byteorder::{ByteOrder, ReadBytesExt, LE};
use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use zerocopy::AsBytes;

use crate::commands::{AdsHeader, Command};
use crate::errors::{ads_error, ErrContext};
use crate::reader::Reader;
use crate::{notif, Source, Timeouts};
use crate::{AmsAddr, AmsNetId, Error, Result};

use zerocopy::byteorder::{U16, U32};

/// Size of the AMS/TCP + AMS headers
// https://infosys.beckhoff.com/content/1033/tc3_ads_intro/115845259.html?id=6032227753916597086
pub(crate) const TCP_HEADER_SIZE: usize = 6;
pub(crate) const AMS_HEADER_SIZE: usize = 38; // including AMS/TCP header
pub(crate) const DEFAULT_BUFFER_SIZE: usize = 100;

/// Represents a connection to a ADS server.
///
/// The Comm's communication methods use `&self`, so that it can be freely
/// shared within one thread, or sent, between threads.  Wrappers such as
/// `Device` or `symbol::Handle` use a `&Client` as well.
#[derive(Debug)]
pub struct Comms {
    /// TCP connection (duplicated with the reader)
    pub socket: TcpStream,
    /// Current invoke ID (identifies the request/reply pair), incremented
    /// after each request
    pub invoke_id: Arc<AtomicU32>,
    /// Read timeout (actually receive timeout for the channel)
    pub read_timeout: Option<Duration>,
    /// The AMS address of the client
    pub source: AmsAddr,
    /// Sender for used Vec buffers to the reader thread
    pub buf_send: Sender<Vec<u8>>,
    /// Receiver for synchronous replies: used in `communicate`
    pub reply_recv: Receiver<Result<Vec<u8>>>,
    /// Receiver for notifications: cloned and given out to interested parties
    pub notif_recv: Receiver<notif::Notification>,
    /// Active notification handles: these will be closed on Drop
    pub notif_handles: RefCell<BTreeSet<(AmsAddr, notif::Handle)>>,
    /// If we opened our local port with the router
    pub source_port_opened: bool,
}

impl Drop for Comms {
    fn drop(&mut self) {
        // Remove our port from the router, if necessary.
        if self.source_port_opened {
            let mut close_port_msg = [1, 0, 2, 0, 0, 0, 0, 0];
            LE::write_u16(&mut close_port_msg[6..], self.source.port());
            let _ = self.socket.write_all(&close_port_msg);
        }

        // Need to shutdown the connection since the socket is duplicated in the
        // reader thread.  This will cause the read() in the thread to return
        // with no data.
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

impl Comms {
    /// Open a new connection to an ADS server.
    ///
    /// If connecting to a server that has an AMS router, it needs to have a
    /// route set for the source IP and NetID, otherwise the connection will be
    /// closed immediately.  The route can be added from TwinCAT, or this
    /// crate's `udp::add_route` helper can be used to add a route via UDP
    /// message.
    ///
    /// `source` is the AMS address to to use as the source; the NetID needs to
    /// match the route entry in the server.  If `Source::Auto`, the NetID is
    /// constructed from the local IP address with .1.1 appended; if there is no
    /// IPv4 address, `127.0.0.1.1.1` is used.
    ///
    /// The AMS port of `source` is not important, as long as it is not a
    /// well-known service port; an ephemeral port number > 49152 is
    /// recommended.  If Auto, the port is set to 58913.
    ///
    /// If you are connecting to the local PLC, you need to set `source` to
    /// `Source::Request`.  This will ask the local AMS router for a new
    /// port and use it as the source port.
    ///
    /// Since all communications is supposed to be handled by an ADS router,
    /// only one TCP/ADS connection can exist between two hosts. Non-TwinCAT
    /// clients should make sure to replicate this behavior, as opening a second
    /// connection will close the first.
    pub fn new(addr: impl ToSocketAddrs, timeouts: Timeouts, source: Source) -> Result<Self> {
        // Connect, taking the timeout into account.  Unfortunately
        // connect_timeout wants a single SocketAddr.
        let addr = addr
            .to_socket_addrs()
            .ctx("converting address to SocketAddr")?
            .next()
            .expect("at least one SocketAddr");
        let mut socket = if let Some(timeout) = timeouts.connect {
            TcpStream::connect_timeout(&addr, timeout).ctx("connecting TCP socket with timeout")?
        } else {
            TcpStream::connect(&addr).ctx("connecting TCP socket")?
        };

        // Disable Nagle to ensure small requests are sent promptly; we're
        // playing ping-pong with request reply, so no pipelining.
        socket.set_nodelay(true).ctx("setting NODELAY")?;
        socket
            .set_write_timeout(timeouts.write)
            .ctx("setting write timeout")?;

        // Determine our source AMS address.  If it's not specified, try to use
        // the socket's local IPv4 address, if it's IPv6 (not sure if Beckhoff
        // devices support that) use `127.0.0.1` as the last resort.
        //
        // If source is Request, send an AMS port open message to the connected
        // router to get our source address.  This is required when connecting
        // via localhost, apparently.
        let mut source_port_opened = false;
        let source = match source {
            Source::Addr(id) => id,
            Source::Auto => {
                let my_addr = socket
                    .local_addr()
                    .ctx("getting local socket address")?
                    .ip();
                if let IpAddr::V4(ip) = my_addr {
                    let [a, b, c, d] = ip.octets();
                    // use some random ephemeral port
                    AmsAddr::new(AmsNetId::new(a, b, c, d, 1, 1), 58913)
                } else {
                    AmsAddr::new(AmsNetId::new(127, 0, 0, 1, 1, 1), 58913)
                }
            }
            Source::Request => {
                let request_port_msg = [0, 16, 2, 0, 0, 0, 0, 0];
                let mut reply = [0; 14];
                socket
                    .write_all(&request_port_msg)
                    .ctx("requesting port from router")?;
                socket
                    .read_exact(&mut reply)
                    .ctx("requesting port from router")?;
                if reply[..6] != [0, 16, 8, 0, 0, 0] {
                    return Err(Error::Reply(
                        "requesting port",
                        "unexpected reply header",
                        0,
                    ));
                }
                source_port_opened = true;
                AmsAddr::new(
                    AmsNetId::from_slice(&reply[6..12]).expect("size"),
                    LE::read_u16(&reply[12..14]),
                )
            }
        };

        // Clone the socket for the reader thread and create our channels for
        // bidirectional communication.
        let socket_clone = socket.try_clone().ctx("cloning TCP socket")?;
        let (buf_send, buf_recv) = bounded(10);
        let (reply_send, reply_recv) = bounded(1);
        let (notif_send, notif_recv) = unbounded();
        let mut source_bytes = [0; 8];
        source.write_to(&mut &mut source_bytes[..]).expect("size");

        // Start the reader thread.
        let reader = Reader {
            socket: socket_clone,
            source: source_bytes,
            buf_recv,
            reply_send,
            notif_send,
        };
        std::thread::spawn(|| reader.run());

        Ok(Comms {
            socket,
            source,
            buf_send,
            reply_recv,
            notif_recv,
            invoke_id: Arc::new(AtomicU32::new(0)),
            read_timeout: timeouts.read,
            notif_handles: RefCell::default(),
            source_port_opened,
        })
    }

    /// Return the source address the client is using.
    pub fn source(&self) -> AmsAddr {
        self.source
    }

    /// Get a receiver for notifications.
    pub fn get_notification_channel(&self) -> Receiver<notif::Notification> {
        self.notif_recv.clone()
    }

    /// Low-level function to execute an ADS command.
    ///
    /// Writes a data from a number of input buffers, and returns data in a
    /// number of output buffers.  The latter might not be filled completely;
    /// the return value specifies the number of total valid bytes.  It is up to
    /// the caller to determine what this means in terms of the passed buffers.
    pub fn communicate(
        &self,
        cmd: Command,
        target: AmsAddr,
        data_in: &[&[u8]],
        data_out: &mut [&mut [u8]],
    ) -> Result<usize> {
        // Increase the invoke ID.  We could also generate a random u32, but
        // this way the sequence of packets can be tracked.
        let invoke_id = self
            .invoke_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // The data we send is the sum of all data_in buffers.
        let data_in_len = data_in.iter().map(|v| v.len()).sum::<usize>();

        // Create outgoing header.
        let ads_data_len = AMS_HEADER_SIZE - TCP_HEADER_SIZE + data_in_len;
        let header = AdsHeader {
            ams_cmd: 0, // send command
            length: U32::new(ads_data_len.try_into()?),
            dest_netid: target.netid(),
            dest_port: U16::new(target.port()),
            src_netid: self.source.netid(),
            src_port: U16::new(self.source.port()),
            command: U16::new(cmd as u16),
            state_flags: U16::new(4), // state flags (4 = send command)
            data_length: U32::new(data_in_len as u32), // overflow checked above
            error_code: U32::new(0),
            invoke_id: U32::new(invoke_id),
        };

        // Collect the outgoing data.  Note, allocating a Vec and calling
        // `socket.write_all` only once is faster than writing in multiple
        // steps, even with TCP_NODELAY.
        let mut request = Vec::with_capacity(ads_data_len);
        request.extend_from_slice(header.as_bytes());
        for buf in data_in {
            request.extend_from_slice(buf);
        }
        // &T impls Write for T: Write, so no &mut self required.
        (&self.socket).write_all(&request).ctx("sending request")?;

        // Get a reply from the reader thread, with timeout or not.
        let reply = if let Some(tmo) = self.read_timeout {
            self.reply_recv
                .recv_timeout(tmo)
                .map_err(|_| io::ErrorKind::TimedOut.into())
                .ctx("receiving reply (route set?)")?
        } else {
            self.reply_recv
                .recv()
                .map_err(|_| io::ErrorKind::UnexpectedEof.into())
                .ctx("receiving reply (route set?)")?
        }?;

        // Validate the incoming reply.  The reader thread already made sure that
        // it is consistent and addressed to us.

        // The source netid/port must match what we sent.
        if reply[14..22] != request[6..14] {
            return Err(Error::Reply(cmd.action(), "unexpected source address", 0));
        }
        // Read the other fields we need.
        assert!(reply.len() >= AMS_HEADER_SIZE);
        let mut ptr = &reply[22..];
        let ret_cmd = ptr.read_u16::<LE>().expect("size");
        let state_flags = ptr.read_u16::<LE>().expect("size");
        let data_len = ptr.read_u32::<LE>().expect("size");
        let error_code = ptr.read_u32::<LE>().expect("size");
        let response_id = ptr.read_u32::<LE>().expect("size");
        let result = if reply.len() >= AMS_HEADER_SIZE + 4 {
            ptr.read_u32::<LE>().expect("size")
        } else {
            0 // this must be because an error code is already set
        };

        // Command must match.
        if ret_cmd != cmd as u16 {
            return Err(Error::Reply(
                cmd.action(),
                "unexpected command",
                ret_cmd.into(),
            ));
        }
        // State flags must be "4 | 1".
        if state_flags != 5 {
            return Err(Error::Reply(
                cmd.action(),
                "unexpected state flags",
                state_flags.into(),
            ));
        }
        // Invoke ID must match what we sent.
        if response_id != invoke_id {
            return Err(Error::Reply(
                cmd.action(),
                "unexpected invoke ID",
                response_id,
            ));
        }
        // Check error code in AMS header.
        if error_code != 0 {
            return ads_error(cmd.action(), error_code);
        }
        // Check result field in payload, only relevant if error_code == 0.
        if result != 0 {
            return ads_error(cmd.action(), result);
        }

        // If we don't want return data, we're done.
        if data_out.is_empty() {
            let _ = self.buf_send.send(reply);
            return Ok(0);
        }

        // Check returned length, it needs to fill at least the first data_out
        // buffer.  This also ensures that we had a result field.
        if (data_len as usize) < data_out[0].len() + 4 {
            return Err(Error::Reply(
                cmd.action(),
                "got less data than expected",
                data_len,
            ));
        }

        // The pure user data length, without the result field.
        let data_len = data_len as usize - 4;

        // Distribute the data into the user output buffers, up to the returned
        // data length.
        let mut offset = AMS_HEADER_SIZE + 4;
        let mut rest_len = data_len;
        for buf in data_out {
            let n = buf.len().min(rest_len);
            buf[..n].copy_from_slice(&reply[offset..][..n]);
            offset += n;
            rest_len -= n;
            if rest_len == 0 {
                break;
            }
        }

        // Send back the Vec buffer to the reader thread.
        let _ = self.buf_send.send(reply);

        // Return either the error or the length of data.
        Ok(data_len)
    }
}
