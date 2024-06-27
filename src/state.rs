use std::{convert::TryFrom, str::FromStr};

/// The ADS state of a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
#[repr(u16)]
pub enum AdsState {
    Invalid = 0,
    Idle = 1,
    Reset = 2,
    Init = 3,
    Start = 4,
    Run = 5,
    Stop = 6,
    SaveCfg = 7,
    LoadCfg = 8,
    PowerFail = 9,
    PowerGood = 10,
    Error = 11,
    Shutdown = 12,
    Suspend = 13,
    Resume = 14,
    Config = 15,
    Reconfig = 16,
    Stopping = 17,
    Incompatible = 18,
    Exception = 19,
}

impl TryFrom<u16> for AdsState {
    type Error = &'static str;

    fn try_from(value: u16) -> std::result::Result<Self, &'static str> {
        Ok(match value {
            0 => Self::Invalid,
            1 => Self::Idle,
            2 => Self::Reset,
            3 => Self::Init,
            4 => Self::Start,
            5 => Self::Run,
            6 => Self::Stop,
            7 => Self::SaveCfg,
            8 => Self::LoadCfg,
            9 => Self::PowerFail,
            10 => Self::PowerGood,
            11 => Self::Error,
            12 => Self::Shutdown,
            13 => Self::Suspend,
            14 => Self::Resume,
            15 => Self::Config,
            16 => Self::Reconfig,
            17 => Self::Stopping,
            18 => Self::Incompatible,
            19 => Self::Exception,
            _ => return Err("invalid state constant"),
        })
    }
}

impl FromStr for AdsState {
    type Err = &'static str;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match &*s.to_ascii_lowercase() {
            "invalid" => Self::Invalid,
            "idle" => Self::Idle,
            "reset" => Self::Reset,
            "init" => Self::Init,
            "start" => Self::Start,
            "run" => Self::Run,
            "stop" => Self::Stop,
            "savecfg" => Self::SaveCfg,
            "loadcfg" => Self::LoadCfg,
            "powerfail" => Self::PowerFail,
            "powergood" => Self::PowerGood,
            "error" => Self::Error,
            "shutdown" => Self::Shutdown,
            "suspend" => Self::Suspend,
            "resume" => Self::Resume,
            "config" => Self::Config,
            "reconfig" => Self::Reconfig,
            "stopping" => Self::Stopping,
            "incompatible" => Self::Incompatible,
            "exception" => Self::Exception,
            _ => return Err("invalid state name"),
        })
    }
}
