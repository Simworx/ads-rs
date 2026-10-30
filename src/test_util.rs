use std::convert::{TryFrom, TryInto};
use std::io::{Read, Write};
use std::mem::size_of;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use byteorder::{ByteOrder, ReadBytesExt, WriteBytesExt, LE};
use zerocopy::byteorder::little_endian::{U32, U64};
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes};

use crate::client::{AddNotif, AdsHeader, ClientReceiver, Command, IndexLength, IndexLengthRW, PendingMap};
use crate::notif::Notification;
use crate::{file, index, AdsState, AmsAddr};

// Handles given out for symbols start here so the first symbol keeps the historic 77.
const SYMBOL_HANDLE_BASE: usize = 77;

/// One request as the fake device received it.
pub struct Request {
    header: AdsHeader,
    /// The ADS payload after the AMS header.
    pub data: Vec<u8>,
}

impl Request {
    /// The ADS command id.
    pub fn command(&self) -> u16 {
        self.header.command.get()
    }

    /// The AMS address this request was sent to.
    pub fn target(&self) -> AmsAddr {
        AmsAddr::new(self.header.dest_netid, self.header.dest_port.get())
    }

    /// Build a notification frame carrying `payload` as its stream back to the sender of this request.
    pub fn notification(&self, payload: &[u8]) -> Vec<u8> {
        let mut frame = self.frame(Command::Notification as u16, 4, payload);
        frame[34..38].copy_from_slice(&[0; 4]);
        frame
    }

    /// Build a frame from the fake device back to the sender of this request.
    pub fn frame(&self, command: u16, state_flags: u16, payload: &[u8]) -> Vec<u8> {
        let mut header = AdsHeader::new_zeroed();
        header.length.set(32 + payload.len() as u32);
        header.dest_netid = self.header.src_netid;
        header.dest_port = self.header.src_port;
        header.src_netid = self.header.dest_netid;
        header.src_port = self.header.dest_port;
        header.command.set(command);
        header.state_flags.set(state_flags);
        header.data_length.set(payload.len() as u32);
        header.invoke_id = self.header.invoke_id;
        [header.as_bytes(), payload].concat()
    }
}

/// What the fake device does with one request.
pub enum Reply {
    /// Answer like the built in device.
    Default,
    /// Write these bytes instead of an answer.
    Frame(Vec<u8>),
    /// Send nothing and keep reading.
    Silent,
    /// Close the connection.
    Close,
}

/// A hook that sees each request before the built in device.
pub type Handler = Box<dyn FnMut(&Request) -> Reply + Send>;

/// How the fake device behaves.
#[derive(Default)]
pub struct Handlers {
    /// Answer with an invalid AMS/TCP reserved field.
    pub garbage_header: bool,
    /// Send notifications that claim `u32::MAX` stamps.
    pub bad_notif: bool,
    /// Answer with invoke id 0.
    pub ignore_invokeid: bool,
    /// Answer this many requests and then go silent.
    pub silent_after: Option<usize>,
    /// Called first for every request.
    pub custom: Option<Handler>,
}

/// An ADS device on a local TCP port for tests.
pub struct FakeServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    stop: AtomicBool,
    stream: Mutex<Option<TcpStream>>,
    device: Mutex<Server>,
}

impl FakeServer {
    /// Bind a free port on 127.0.0.1 and serve one client at a time.
    pub fn start(handlers: Handlers) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            stream: Mutex::new(None),
            device: Mutex::new(Server::new(handlers)),
        });
        let worker = shared.clone();
        let thread = thread::spawn(move || {
            while !worker.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((socket, _)) => {
                        let _ = socket.set_nonblocking(false);
                        *worker.stream.lock().unwrap_or_else(PoisonError::into_inner) =
                            socket.try_clone().ok();
                        // A stop that took the stream before it was stored would otherwise never end this serve
                        if !worker.stop.load(Ordering::SeqCst) {
                            worker.serve(socket);
                        }
                        worker.close();
                    }
                    Err(_) => thread::sleep(Duration::from_millis(10)),
                }
            }
        });
        Ok(Self { addr, shared, thread: Some(thread) })
    }

    /// The address clients connect to.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Add a symbol that `Handle::new` finds by name with `value` as its data.
    pub fn add_symbol(&self, name: &str, value: &[u8]) {
        self.shared.device().symbols.push((name.into(), value.into()));
    }

    /// Set the ADS and device state that `get_state` reports.
    pub fn set_state(&self, state: AdsState, dev_state: u16) {
        self.shared.device().state = (state, dev_state);
    }

    /// Drop the client connection and stop serving.
    pub fn stop(mut self) {
        self.halt();
    }

    fn halt(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.halt();
    }
}

impl Shared {
    fn close(&self) {
        if let Some(stream) = self.stream.lock().unwrap_or_else(PoisonError::into_inner).take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn device(&self) -> MutexGuard<'_, Server> {
        self.device.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn serve(&self, mut socket: TcpStream) {
        loop {
            let mut header = AdsHeader::new_zeroed();
            if socket.read_exact(header.as_mut_bytes()).is_err() {
                return;
            }
            let mut data = vec![0; header.data_length.get() as usize];
            if socket.read_exact(&mut data).is_err() {
                return;
            }
            let Some(bytes) = self.device().answer(&Request { header, data }) else {
                return;
            };
            if socket.write_all(&bytes).is_err() {
                return;
            }
        }
    }
}

struct Server {
    handlers: Handlers,
    answered: usize,
    data: Vec<u8>,
    // If the test file is opened for writing, and the read/write position.
    file_ptr: Option<(bool, usize)>,
    // If a notification has been added, the (offset, size) to send.
    notif: Option<(usize, usize)>,
    // The simulated device state.
    state: (AdsState, u16),
    symbols: Vec<(String, Vec<u8>)>,
}

impl Server {
    fn new(handlers: Handlers) -> Self {
        Self {
            handlers,
            answered: 0,
            data: vec![0; 1024],
            file_ptr: None,
            notif: None,
            state: (AdsState::Run, 0),
            symbols: Vec::new(),
        }
    }

    fn answer(&mut self, request: &Request) -> Option<Vec<u8>> {
        if let Some(custom) = &mut self.handlers.custom {
            match custom(request) {
                Reply::Default => (),
                Reply::Frame(bytes) => return Some(bytes),
                Reply::Silent => return Some(Vec::new()),
                Reply::Close => return None,
            }
        }
        if self.handlers.silent_after.is_some_and(|n| self.answered >= n) {
            return Some(Vec::new());
        }
        self.answered += 1;

        let data = &request.data;
        let (reply_data, error) = match request.command() {
            1 => self.do_devinfo(data),
            2 => self.do_read(data),
            3 => self.do_write(data),
            4 => self.do_read_state(data),
            5 => self.do_write_control(data),
            6 => self.do_add_notif(data),
            7 => self.do_del_notif(data),
            9 => self.do_read_write(data),
            _ => (vec![], 0x701),
        };

        let mut out = match self.notif {
            Some((off, len)) => self.notification(off, len, request),
            None => Vec::new(),
        };
        let mut reply = AdsHeader::read_from_bytes(
            &request.frame(request.command(), request.header.state_flags.get() | 1, &[])[..],
        )
        .unwrap();
        reply.length.set(32 + reply_data.len() as u32);
        reply.data_length.set(reply_data.len() as u32);
        reply.error_code.set(error);
        if self.handlers.garbage_header {
            reply.ams_cmd = 234;
        }
        if self.handlers.ignore_invokeid {
            reply.invoke_id.set(0);
        }
        out.extend(reply.as_bytes());
        out.extend(reply_data);
        Some(out)
    }

    fn notification(&self, off: usize, len: usize, request: &Request) -> Vec<u8> {
        let mut notif = SingleNotification::default();
        notif.len.set((size_of::<SingleNotification>() + len) as u32 - 4);
        notif.stamps.set(if self.handlers.bad_notif { u32::MAX } else { 1 });
        notif.stamp.set(0x9988776655443322);
        notif.samples.set(1);
        notif.handle.set(132);
        notif.size.set(len as u32);
        request.notification(&[notif.as_bytes(), &self.data[off..][..len]].concat())
    }

    fn symbol(&self, handle: usize) -> Option<&(String, Vec<u8>)> {
        self.symbols.get(handle.wrapping_sub(SYMBOL_HANDLE_BASE))
    }

    fn do_devinfo(&self, data: &[u8]) -> (Vec<u8>, u32) {
        if !data.is_empty() {
            return (vec![], 0x706);
        }
        // no error, major 7, minor 1
        let mut out = 0u32.to_le_bytes().to_vec();
        out.write_u8(7).unwrap();
        out.write_u8(1).unwrap();
        out.write_u16::<LE>(4024).unwrap();
        out.extend(b"Nice device\0\0\0\0\0");
        (out, 0)
    }

    fn do_read_state(&self, data: &[u8]) -> (Vec<u8>, u32) {
        if !data.is_empty() {
            return (vec![], 0x706);
        }
        let mut out = 0u32.to_le_bytes().to_vec();
        out.write_u16::<LE>(self.state.0 as u16).unwrap();
        out.write_u16::<LE>(self.state.1).unwrap();
        (out, 0)
    }

    fn do_write_control(&mut self, mut data: &[u8]) -> (Vec<u8>, u32) {
        if data.len() != 8 {
            return (vec![], 0x706);
        }
        let adsstate = data.read_u16::<LE>().unwrap();
        let devstate = data.read_u16::<LE>().unwrap();
        let mut out = vec![];
        match crate::AdsState::try_from(adsstate) {
            Err(_) | Ok(crate::AdsState::Invalid) => {
                out.write_u32::<LE>(0x70B).unwrap();
            }
            Ok(adsstate) => {
                self.state = (adsstate, devstate);
                out.write_u32::<LE>(0).unwrap();
            }
        }
        (out, 0)
    }

    fn do_read(&self, data: &[u8]) -> (Vec<u8>, u32) {
        if data.len() != size_of::<IndexLength>() {
            return (vec![], 0x706);
        }
        let request = IndexLength::read_from_bytes(data).unwrap();
        let grp = request.index_group.get();
        let off = request.index_offset.get() as usize;
        let len = request.length.get() as usize;
        let mut out = Vec::new();
        out.write_u32::<LE>(0).unwrap();
        // Simulate symbol access.
        if grp == index::RW_SYMVAL_BYHANDLE {
            let Some((_, value)) = self.symbol(off) else {
                return (vec![], 0x710);
            };
            if len != value.len() {
                return (vec![], 0x705);
            }
            out.write_u32::<LE>(request.length.get()).unwrap();
            out.extend(value);
            return (out, 0);
        } else if grp != index::PLC_RW_M {
            return (vec![], 0x702);
        }
        if off + len > self.data.len() {
            return (vec![], 0x703);
        }
        out.write_u32::<LE>(request.length.get()).unwrap();
        out.extend(&self.data[off..][..len]);
        (out, 0)
    }

    fn do_write(&mut self, data: &[u8]) -> (Vec<u8>, u32) {
        if data.len() < size_of::<IndexLength>() {
            return (vec![], 0x706);
        }
        let request = IndexLength::read_from_bytes(&data[..12]).unwrap();
        let grp = request.index_group.get();
        let off = request.index_offset.get() as usize;
        let len = request.length.get() as usize;

        if grp == index::RW_SYMVAL_BYHANDLE {
            let Some((_, value)) = self.symbols.get_mut(off.wrapping_sub(SYMBOL_HANDLE_BASE)) else {
                return (vec![], 0x710);
            };
            if len != value.len() || data.len() != size_of::<IndexLength>() + len {
                return (vec![], 0x705);
            }
            value.copy_from_slice(&data[12..]);
            return (0u32.to_le_bytes().into(), 0);
        } else if grp == index::RELEASE_SYMHANDLE {
            if data.len() != 16 || self.symbol(LE::read_u32(&data[12..]) as usize).is_none() {
                return (vec![], 0x710);
            }
            return (0u32.to_le_bytes().into(), 0);
        } else if grp != index::PLC_RW_M {
            return (vec![], 0x702);
        }
        if off + len > self.data.len() {
            return (vec![], 0x703);
        }
        if data.len() != size_of::<IndexLength>() + len {
            return (vec![], 0x706);
        }
        self.data[off..][..len].copy_from_slice(&data[12..]);
        (0u32.to_le_bytes().into(), 0)
    }

    fn do_read_write(&mut self, data: &[u8]) -> (Vec<u8>, u32) {
        if data.len() < size_of::<IndexLengthRW>() {
            return (vec![], 0x706);
        }
        let request = IndexLengthRW::read_from_bytes(&data[..16]).unwrap();
        let off = request.index_offset.get();
        let rlen = request.read_length.get() as usize;
        let wlen = request.write_length.get() as usize;
        let mut out = 0u32.to_le_bytes().to_vec();

        // Simulate file and symbol access.
        match request.index_group.get() {
            index::SUMUP_READ_EX => {
                let mut mdata: Vec<u8> = vec![];
                let mut rdata: Vec<u8> = vec![];
                for i in 0..off as usize {
                    let rlen = LE::read_u32(&data[16 + i * 12 + 8..]) as usize;
                    let (mut d, e) = self.do_read(&data[16 + i * 12..][..12]);
                    mdata.write_u32::<LE>(e).unwrap();
                    d.resize(rlen + 8, 0);
                    mdata.write_u32::<LE>(d.len() as u32 - 8).unwrap();
                    rdata.extend(&d[8..]);
                }
                out.write_u32::<LE>((mdata.len() + rdata.len()) as u32).unwrap();
                out.extend(mdata);
                out.extend(rdata);
            }
            index::SUMUP_WRITE => {
                let mut woff = 16 + off as usize * 12;
                out.write_u32::<LE>(4 * off).unwrap();
                for i in 0..off as usize {
                    let wlen = LE::read_u32(&data[16 + i * 12 + 8..]) as usize;
                    let mut subdata = data[16 + i * 12..][..12].to_vec();
                    subdata.extend(&data[woff..][..wlen]);
                    woff += wlen;
                    let (_, e) = self.do_write(&subdata);
                    out.write_u32::<LE>(e).unwrap();
                }
            }
            index::SUMUP_READWRITE => {
                let mut mdata: Vec<u8> = vec![];
                let mut rdata: Vec<u8> = vec![];
                let mut woff = 16 + off as usize * 16;
                for i in 0..off as usize {
                    let wlen = LE::read_u32(&data[16 + i * 16 + 12..]) as usize;
                    let mut subdata = data[16 + i * 16..][..16].to_vec();
                    subdata.extend(&data[woff..][..wlen]);
                    woff += wlen;
                    let (d, e) = self.do_read_write(&subdata);
                    mdata.write_u32::<LE>(e).unwrap();
                    if d.len() > 8 {
                        mdata.write_u32::<LE>(d.len() as u32 - 8).unwrap();
                        rdata.extend(&d[8..]);
                    } else {
                        mdata.write_u32::<LE>(0).unwrap();
                    }
                }
                out.write_u32::<LE>((mdata.len() + rdata.len()) as u32).unwrap();
                out.extend(mdata);
                out.extend(rdata);
            }
            index::SUMUP_ADDDEVNOTE => {
                out.write_u32::<LE>(8 * off).unwrap();
                for i in 0..off as usize {
                    let (d, e) = self.do_add_notif(&data[16 + i * 40..][..40]);
                    out.write_u32::<LE>(e).unwrap();
                    if d.len() > 4 {
                        out.extend(&d[4..]);
                    } else {
                        out.write_u32::<LE>(0).unwrap();
                    }
                }
            }
            index::SUMUP_DELDEVNOTE => {
                out.write_u32::<LE>(4 * off).unwrap();
                for i in 0..off as usize {
                    let (_, e) = self.do_del_notif(&data[16 + i * 4..][..4]);
                    out.write_u32::<LE>(e).unwrap();
                }
            }
            index::FILE_OPEN => {
                if &data[16..] != b"/etc/passwd" {
                    return (vec![], 0x70C);
                }
                if self.file_ptr.is_some() {
                    return (vec![], 0x708);
                }
                let write = off & (file::WRITE | file::APPEND) != 0;
                out.write_u32::<LE>(4).unwrap();
                out.write_u32::<LE>(42).unwrap();
                self.file_ptr = Some((write, 0));
            }
            index::FILE_CLOSE => {
                if !data[16..].is_empty() {
                    return (vec![], 0x70B);
                }
                if off != 42 {
                    return (vec![], 0x70C);
                }
                out.write_u32::<LE>(0).unwrap();
                self.file_ptr = None;
            }
            index::FILE_WRITE => {
                if let Some((true, ptr)) = &mut self.file_ptr {
                    out.write_u32::<LE>(0).unwrap();
                    *ptr += wlen;
                } else {
                    return (vec![], 0x704);
                }
            }
            index::FILE_READ => {
                if let Some((false, ptr)) = &mut self.file_ptr {
                    let cur = *ptr;
                    *ptr = (*ptr + rlen).min(888);
                    let amount = *ptr - cur;
                    out.write_u32::<LE>(amount as u32).unwrap();
                    out.resize(out.len() + amount, 0);
                } else {
                    return (vec![], 0x704);
                }
            }
            index::FILE_DELETE => {
                if &data[16..] != b"/etc/passwd" {
                    return (vec![], 0x70C);
                }
                if self.file_ptr.is_some() {
                    // send an unknown error number
                    return (vec![], 0xFFFF);
                }
                out.write_u32::<LE>(0).unwrap();
            }
            index::GET_SYMHANDLE_BYNAME => {
                let Some(pos) = self.symbols.iter().position(|(name, _)| name.as_bytes() == &data[16..])
                else {
                    return (vec![], 0x710);
                };
                out.write_u32::<LE>(4).unwrap();
                out.write_u32::<LE>((SYMBOL_HANDLE_BASE + pos) as u32).unwrap();
            }
            _ => return (vec![], 0x702),
        }
        (out, 0)
    }

    fn do_add_notif(&mut self, data: &[u8]) -> (Vec<u8>, u32) {
        if data.len() != size_of::<AddNotif>() {
            return (vec![], 0x706);
        }
        let request = AddNotif::read_from_bytes(data).unwrap();
        let off = request.index_offset.get() as usize;
        let len = request.length.get() as usize;

        if request.index_group.get() != index::PLC_RW_M {
            return (vec![], 0x702);
        }
        if off + len > self.data.len() {
            return (vec![], 0x703);
        }
        self.notif = Some((off, len));
        let mut out = 0u32.to_le_bytes().to_vec();
        out.write_u32::<LE>(132).unwrap(); // handle
        (out, 0)
    }

    fn do_del_notif(&mut self, data: &[u8]) -> (Vec<u8>, u32) {
        if data.len() != 4 {
            return (vec![], 0x706);
        }
        let handle = u32::from_le_bytes(data.try_into().unwrap());
        if handle != 132 {
            return (vec![], 0x714);
        }
        if self.notif.is_none() {
            return (vec![], 0x714);
        }
        self.notif = None;
        (0u32.to_le_bytes().into(), 0)
    }
}

#[derive(FromBytes, IntoBytes, Immutable, Debug, Default)]
#[repr(C)]
struct SingleNotification {
    len: U32,
    stamps: U32,
    stamp: U64,
    samples: U32,
    handle: U32,
    size: U32,
}

/// Run the client's frame reader over `bytes` as if a server had sent them.
///
/// Returns the notifications it delivered and the error that stopped it.
pub fn split_frames(mut bytes: &[u8]) -> (Vec<Notification>, crate::Result<()>) {
    let (mut tx, rx) = crossbeam_channel::unbounded();
    let result =
        ClientReceiver::reader_work(crate::AmsAddr::default(), PendingMap::default(), &mut bytes, &mut tx);
    (rx.try_iter().collect(), result)
}
