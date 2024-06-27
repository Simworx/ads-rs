use std::time::Duration;

/// Holds the different timeouts that will be used by the Client.
/// None means no timeout in every case.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// Connect timeout
    pub connect: Option<Duration>,
    /// Reply read timeout
    pub read: Option<Duration>,
    /// Socket write timoeut
    pub write: Option<Duration>,
}

impl Timeouts {
    /// Create a new `Timeouts` where all values are identical.
    pub fn new(duration: Duration) -> Self {
        Self {
            connect: Some(duration),
            read: Some(duration),
            write: Some(duration),
        }
    }

    /// Create a new `Timeouts` without any timeouts specified.
    pub fn none() -> Self {
        Self {
            connect: None,
            read: None,
            write: None,
        }
    }
}
