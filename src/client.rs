#![allow(missing_docs)]

use std::{
    collections::HashMap,
    net::ToSocketAddrs,
    sync::{Arc, Mutex},
};

use crossbeam_channel::Receiver;

use crate::{errors::*, notif, AmsAddr, AmsNetId, Comms, Device, Source, Timeouts};

/// TODO: Document.

pub struct Client {
    comms: Arc<Comms>,
    devices: Mutex<HashMap<AmsAddr, Arc<Device>>>,
}

impl Client {
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
        let comms = Comms::new(addr, timeouts, source);

        match comms {
            Ok(c) => Ok(Client {
                comms: Arc::new(c),
                devices: Mutex::new(HashMap::new()),
            }),
            Err(e) => Err(e),
        }
    }

    /// Return a wrapper that executes operations for a target device (known by
    /// NetID and port).
    ///
    /// The local NetID `127.0.0.1.1.1` is mapped to the client's source NetID,
    /// so that you can connect to a local PLC using:
    ///
    /// ```rust,ignore
    /// let client = Client::new("127.0.0.1", ..., Source::Request);
    /// let device = client.device(AmsAddr::new(AmsNetId::local(), 851));
    /// ```
    ///
    /// without knowing its NetID.
    pub fn device(&self, mut addr: AmsAddr) -> Result<Arc<Device>> {
        if addr.netid() == AmsNetId::local() {
            addr = AmsAddr::new(self.comms.source().netid(), addr.port());
        }

        let mut devices: std::sync::MutexGuard<HashMap<AmsAddr, Arc<Device>>> =
            match self.devices.lock() {
                Ok(d) => d,
                Err(_) => return Err(Error::Locking("devices")),
            };

        Ok(devices
            .entry(addr)
            .or_insert(Device::new(self.comms.clone(), addr))
            .clone())
    }

    /// Return the source address the client is using.
    pub fn source(&self) -> AmsAddr {
        self.comms.source()
    }

    /// Get a receiver for notifications.
    pub fn get_notification_channel(&self) -> Receiver<notif::Notification> {
        self.comms.notif_recv.clone()
    }
}
