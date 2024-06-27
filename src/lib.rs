//! Work with PLCs using the ADS protocol
//!
//! # Introduction
//!
//! ADS is the native protocol used by programmable logic controllers (PLCs) and
//! the TwinCAT automation system produced by [Beckhoff GmbH](https://www.beckhoff.com/).
//!
//! The [specification](https://infosys.beckhoff.de/content/1031/tc3_adscommon/html/tcadscommon_introads.htm)
//! can be found on their Information System pages.
//!
//! # Example
//!
//! ```rust,ignore
//! // Open a connection to an ADS device identified by hostname/IP and port.
//! // For TwinCAT devices, a route must be set to allow the client to connect.
//! // The source AMS address is automatically generated from the local IP,
//! // but can be explicitly specified as the third argument.
//! let client = ads::Client::new(("plchost", ads::PORT), ads::Timeouts::none(),
//!                               ads::Source::Auto)?;
//!
//! // Specify the target ADS device to talk to, by NetID and AMS port.
//! // Port 851 usually refers to the first PLC instance.
//! let device = client.device(ads::AmsAddr::new([5, 32, 116, 5, 1, 1].into(), 851));
//!
//! // Ensure that the PLC instance is running.
//! assert!(device.get_state()?.0 == ads::AdsState::Run);
//!
//! // Request a handle to a named symbol in the PLC instance.
//! let handle = ads::Handle::new(device, "MY_SYMBOL")?;
//!
//! // Read data in form of an u32 from the handle.
//! let value: u32 = handle.read_value()?;
//! println!("MY_SYMBOL value is {}", value);
//! ```

#![deny(missing_docs)]
#![cfg_attr(not(test), deny(clippy::unwrap_used))]

pub mod client;
mod commands;
pub mod comms;
pub mod device;
pub mod errors;
pub mod file;
pub mod index;
pub mod netid;
pub mod notif;
pub mod ports;
mod reader;
mod source;
mod state;
pub mod strings;
pub mod symbol;
#[cfg(test)]
mod test;
mod timeouts;
pub mod udp;
mod utils;

pub use comms::Comms;
pub use device::Device;
pub use errors::{Error, Result};
pub use file::File;
pub use netid::{AmsAddr, AmsNetId, AmsPort};
pub use source::Source;
pub use state::AdsState;
pub use symbol::Handle;
pub use timeouts::Timeouts;

/// The default port for TCP communication.
pub const PORT: u16 = 0xBF02;
/// The default port for UDP communication.
pub const UDP_PORT: u16 = 0xBF03;
