use crate::{
    commands::Command,
    comms::{AMS_HEADER_SIZE, DEFAULT_BUFFER_SIZE, TCP_HEADER_SIZE},
    errors::{ErrContext, Result},
    notif, Error,
};
use crossbeam_channel::{Receiver, Sender};
use std::{
    io::Read,
    net::{Shutdown, TcpStream},
};
use zerocopy::{ByteOrder, LE};
// Implementation detail: reader thread that takes replies and notifications
// and distributes them accordingly.
pub struct Reader {
    pub socket: TcpStream,
    pub source: [u8; 8],
    pub buf_recv: Receiver<Vec<u8>>,
    pub reply_send: Sender<Result<Vec<u8>>>,
    pub notif_send: Sender<notif::Notification>,
}

impl Reader {
    pub fn run(mut self) {
        self.run_inner();
        // We can't do much here.  But try to shut down the socket so that
        // the main client can't be used anymore either.
        let _ = self.socket.shutdown(Shutdown::Both);
    }

    fn run_inner(&mut self) {
        loop {
            // Get a buffer from the free-channel or create a new one.
            let mut buf = self
                .buf_recv
                .try_recv()
                .unwrap_or_else(|_| Vec::with_capacity(DEFAULT_BUFFER_SIZE));

            // Read a header from the socket.
            buf.resize(TCP_HEADER_SIZE, 0);
            if self
                .socket
                .read_exact(&mut buf)
                .ctx("reading AMS packet header")
                .is_err()
            {
                // Not sending an error back; we don't know if something was
                // requested or the socket was just closed from either side.
                return;
            }

            // Read the rest of the packet.
            let packet_length = LE::read_u32(&buf[2..6]) as usize;
            buf.resize(TCP_HEADER_SIZE + packet_length, 0);
            if let Err(e) = self
                .socket
                .read_exact(&mut buf[6..])
                .ctx("reading rest of packet")
            {
                let _ = self.reply_send.send(Err(e));
                return;
            }

            // Is it something other than an ADS command packet?
            let ams_cmd = LE::read_u16(&buf);
            if ams_cmd != 0 {
                // if it's a known packet type, continue
                if matches!(ams_cmd, 1 | 4096 | 4097 | 4098) {
                    continue;
                }
                let _ = self.reply_send.send(Err(Error::Reply(
                    "reading packet",
                    "invalid packet or unknown AMS command",
                    ams_cmd as _,
                )));
                return;
            }

            // If the header length fields aren't self-consistent, abort the connection.
            let rest_length = LE::read_u32(&buf[26..30]) as usize;
            if rest_length != packet_length + TCP_HEADER_SIZE - AMS_HEADER_SIZE {
                let _ = self.reply_send.send(Err(Error::Reply(
                    "reading packet",
                    "inconsistent packet",
                    0,
                )));
                return;
            }

            // Check that the packet is meant for us.
            if buf[6..14] != self.source {
                continue;
            }

            // If it looks like a reply, send it back to the requesting thread,
            // it will handle further validation.
            if LE::read_u16(&buf[22..24]) != Command::Notification as u16 {
                if self.reply_send.send(Ok(buf)).is_err() {
                    // Client must have been shut down.
                    return;
                }
                continue;
            }

            // Validate notification message fields.
            let state_flags = LE::read_u16(&buf[24..26]);
            let error_code = LE::read_u32(&buf[30..34]);
            let length = LE::read_u32(&buf[38..42]) as usize;
            if state_flags != 4 || error_code != 0 || length != rest_length - 4 || length < 4 {
                continue;
            }

            // Send the notification to whoever wants to receive it.
            if let Ok(notif) = notif::Notification::new(buf) {
                self.notif_send.send(notif).expect("never disconnects");
            }
        }
    }
}
