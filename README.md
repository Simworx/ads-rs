# `ads`

[![Build status](https://github.com/birkenfeld/ads-rs/actions/workflows/main.yml/badge.svg)](https://github.com/birkenfeld/ads-rs)
[![crates.io](https://img.shields.io/crates/v/ads.svg)](https://crates.io/crates/ads)
[![docs.rs](https://img.shields.io/docsrs/ads)](https://docs.rs/ads)

This crate allows to connect to [Beckhoff](https://beckhoff.com) TwinCAT devices
and other servers speaking the ADS (Automation Device Specification) protocol.

## About this fork and upstream contributions

This fork builds on [birkenfeld/ads-rs](https://github.com/birkenfeld/ads-rs).
We appreciate the original authors' work and the foundation it provides for our
TwinCAT integration.

The requirements of Simworx's in-house software have taken this fork
far enough from upstream that we are not currently proposing the full set of
changes for inclusion. Reviewing, integrating, and maintaining these additions
would represent a substantial commitment, and that extra scope may not fit the
upstream maintainers' priorities or available time. This is our decision about
how to maintain our extensions, rather than a statement that upstream has
rejected them. Small, independently useful fixes may still be suitable for
upstream contributions where there is interest.

The fork's additions include:

- More complete TwinCAT symbol and type information, including fields,
  attributes, enum variants, and RPC metadata.
- Caller-provided IDs on multi-notification requests, so results can be matched
  to the application objects that requested them.
- PLC task information, including task timing and cycle counters, with readers
  that use the target's type metadata to interpret the layout.

We expect this direction to continue. Planned work includes more PLC and
EtherCAT diagnostics, further refinements to client sharing and synchronisation
for our `Send`/`Sync` use cases, and type-safe notification data APIs. These are
extensions for our integration needs, and further work will be guided by
experience using the library in Simworx's in-house software.

The badges above and the installation example below refer to the upstream release;
the fork-specific additions require a suitable revision of this repository.

## Installation

Use with Cargo as usual, no system dependencies are required.

```toml
[dependencies]
ads = "0.7"
```

### Rust version

Minimum supported Rust version is 1.92.0.

## Usage

A simple example:

```rust
fn main() -> ads::Result<()> {
    // Open a connection to an ADS device identified by hostname/IP and port.
    // For TwinCAT devices, a route must be set to allow the client to connect.
    // The source AMS address is automatically generated from the local IP,
    // but can be explicitly specified as the third argument.
    let client = ads::Client::new(("plchost", ads::PORT), ads::Timeouts::none(),
                                  ads::Source::Auto)?;
    // On Windows, when connecting to a TwinCAT instance running on the same
    // machine, use the following to connect:
    let client = ads::Client::new(("127.0.0.1", ads::PORT), ads::Timeouts::none(),
                                  ads::Source::Request)?;

    // Specify the target ADS device to talk to, by NetID and AMS port.
    // Port 851 usually refers to the first PLC instance.
    let device = client.device(ads::AmsAddr::new([5, 32, 116, 5, 1, 1].into(), 851));

    // Ensure that the PLC instance is running.
    assert!(device.get_state()?.0 == ads::AdsState::Run);

    // Request a handle to a named symbol in the PLC instance.
    let handle = Handle::new(device, "MY_SYMBOL")?;

    // Read data in form of an u32 from the handle.
    let value: u32 = handle.read_value()?;
    println!("MY_SYMBOL value is {}", value);

    // Connection will be closed when the client is dropped.
    Ok(())
}
```

## Features

All ADS requests are implemented.

Further features include support for receiving notifications from a channel,
file access via ADS, and communication via UDP to identify an ADS system and set
routes automatically.

## Examples

A utility called `adstool` is found under `examples/`, very similar to the one
provided by [the C++ library](https://github.com/Beckhoff/ADS).

