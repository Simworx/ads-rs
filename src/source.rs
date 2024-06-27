use crate::AmsAddr;

/// Specifies the source AMS address to use.
#[derive(Clone, Copy, Debug)]
pub enum Source {
    /// Auto-generate a source address from the local address and a random port.
    Auto,
    /// Use a specified source address.
    Addr(AmsAddr),
    /// Request to open a port in the connected router and get the address from
    /// it.  This is necessary when connecting to a local PLC on `127.0.0.1`.
    Request,
}
