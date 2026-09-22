//! TwinCAT 3 PLC task telemetry.
//!
//! [`TaskInfoReader`] is a reusable description of the task-info symbols and
//! their validated element layout. Construct it while resolving symbols, retain
//! it for repeated samples, and recreate it when that symbol metadata changes.

use std::convert::TryInto;
use std::time::Duration;

use crate::{Device, Error, Result};

/// The `_TaskInfo` symbol exposed by `Tc2_System`.
pub const TASK_INFO_SYMBOL: &str = "TwinCAT_SystemInfoVarList._TaskInfo";

/// The `_AppInfo` symbol exposed by `Tc2_System`.
pub const APP_INFO_SYMBOL: &str = "TwinCAT_SystemInfoVarList._AppInfo";

const TASK_PREFIX_SIZE: usize = 32;
const TASK_NAME_SIZE: usize = 64;
const APP_INFO_TASK_COUNT_SIZE: usize = 8;
const APP_INFO_TASK_COUNT_OFFSET: usize = 4;
const TICK_NANOS: u64 = 100;
const MAX_TASK_INFO_SIZE: usize = 16 * 1024 * 1024;

/// A resolved ADS memory range.
///
/// The range must come from the same symbol metadata generation as the task
/// layout used to construct a [`TaskInfoReader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskInfoLocation {
    /// ADS index group.
    pub index_group: u32,
    /// ADS index offset.
    pub index_offset: u32,
    /// Number of bytes in the symbol.
    pub size: usize,
}

/// The validated layout of one `_TaskInfo` array entry.
///
/// Build this from the entry type in the caller's symbol metadata cache. The
/// required TwinCAT fields occupy the documented 32-byte prefix. Task names are
/// decoded only after the caller has validated the `TaskName: STRING(63)` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskInfoLayout {
    entry_size: usize,
    task_name_offset: Option<usize>,
}

impl TaskInfoLayout {
    /// Create a layout for an entry with the supplied size and no optional name.
    pub fn new(entry_size: usize) -> Result<Self> {
        if entry_size < TASK_PREFIX_SIZE {
            return Err(Error::Reply(
                "creating task info layout",
                "task entry is shorter than documented prefix",
                entry_size as u32,
            ));
        }
        Ok(Self { entry_size, task_name_offset: None })
    }

    /// Decode a validated inline `TaskName: STRING(63)` field at `offset`.
    ///
    /// Call this only after inspecting type metadata and confirming the field's
    /// type and 64-byte size. It rejects a field that does not fit the entry.
    pub fn with_task_name(mut self, offset: usize) -> Result<Self> {
        if offset < TASK_PREFIX_SIZE
            || offset.checked_add(TASK_NAME_SIZE).map_or(true, |end| end > self.entry_size)
        {
            return Err(Error::Reply(
                "creating task info layout",
                "task name field does not fit task entry",
                offset as u32,
            ));
        }
        self.task_name_offset = Some(offset);
        Ok(self)
    }

    /// Return the byte size of one task entry.
    #[must_use]
    pub fn entry_size(&self) -> usize {
        self.entry_size
    }
}

/// A configured TwinCAT task, as reported by `_TaskInfo`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlcTaskSystemInfo {
    /// Original PLC array index. This is one-based by default; use
    /// [`TaskInfoReader::with_first_index`] for another array lower bound.
    pub index: i32,
    /// TwinCAT object identifier.
    pub object_id: u32,
    /// The task's ADS port.
    pub ads_port: u16,
    /// Real-time priority. Lower values run first.
    pub priority: u16,
    /// Configured cycle time.
    pub cycle_time: Duration,
    /// Duration of the last completed cycle.
    pub last_exec_time: Duration,
    /// Distributed-clock task time in 100 ns ticks.
    pub dc_task_time: i64,
    /// Wrapping completed-cycle counter.
    pub cycle_count: u32,
    /// Whether the task is in its first cycle after a start.
    pub first_cycle: bool,
    /// Whether the runtime flagged the sampled cycle as over time.
    pub cycle_time_exceeded: bool,
    /// Whether the task runs after the output update.
    pub in_call_after_output_update: bool,
    /// Whether the real-time framework flagged a violation.
    pub rt_violation: bool,
    /// Task name when the validated entry layout includes `STRING(63)`.
    pub name: Option<String>,
}

impl PlcTaskSystemInfo {
    /// Return the fraction of the configured cycle consumed by the last cycle.
    #[must_use]
    pub fn utilisation(&self) -> Option<f32> {
        let cycle = self.cycle_time.as_secs_f32();
        if cycle > 0.0 {
            Some(self.last_exec_time.as_secs_f32() / cycle)
        } else {
            None
        }
    }
}

/// The result of reading `_AppInfo.TaskCnt` alongside task telemetry.
#[derive(Debug)]
pub enum ReportedTaskCount {
    /// `_AppInfo.TaskCnt` was read successfully.
    Available(u32),
    /// Task data was read successfully, but `_AppInfo.TaskCnt` could not be
    /// read. The contained ADS error lets consumers report incomplete coverage.
    Unavailable(Error),
}

/// One task sample and its optional application-wide task count.
#[derive(Debug)]
pub struct PlcTaskInfoSnapshot {
    /// The application's reported task count, when an app-info location was
    /// supplied to the reader.
    pub reported_task_count: Option<ReportedTaskCount>,
    /// Populated task entries, retaining their original array indices.
    pub tasks: Vec<PlcTaskSystemInfo>,
}

impl PlcTaskInfoSnapshot {
    /// Return the number of tasks the fixed `_TaskInfo` array cannot expose.
    #[must_use]
    pub fn unmonitored_task_count(&self) -> Option<u32> {
        match self.reported_task_count.as_ref()? {
            ReportedTaskCount::Available(count) => Some(count.saturating_sub(self.tasks.len() as u32)),
            ReportedTaskCount::Unavailable(_) => None,
        }
    }
}

/// Cached, validated metadata used to read TwinCAT task telemetry.
///
/// `TaskInfoReader` does not discover symbols. Pass locations from an uploaded
/// symbol table or another caller-owned cache, then reuse this value for each
/// sample. Recreate it after reconnecting, downloading a project, an online
/// change, or any other symbol metadata refresh.
#[derive(Debug, Clone, Copy)]
pub struct TaskInfoReader {
    task_info: TaskInfoLocation,
    app_info: Option<TaskInfoLocation>,
    layout: TaskInfoLayout,
    first_index: i32,
}

impl TaskInfoReader {
    /// Create a reader for a resolved `_TaskInfo` location and its array element
    /// layout from type metadata.
    ///
    /// The layout's element size must divide the complete task symbol size.
    pub fn new(task_info: TaskInfoLocation, layout: TaskInfoLayout) -> Result<Self> {
        if task_info.size == 0
            || task_info.size > MAX_TASK_INFO_SIZE
            || task_info.size % layout.entry_size != 0
        {
            return Err(Error::Reply(
                "creating task info reader",
                "unsupported task-info layout",
                task_info.size as u32,
            ));
        }
        Ok(Self { task_info, app_info: None, layout, first_index: 1 })
    }

    /// Include a resolved `_AppInfo` location so [`Self::read`] also attempts to
    /// obtain `TaskCnt`. A failure there leaves task data available in the snapshot.
    pub fn with_app_info(mut self, app_info: TaskInfoLocation) -> Result<Self> {
        if app_info.size < APP_INFO_TASK_COUNT_SIZE {
            return Err(Error::Reply(
                "creating task info reader",
                "app-info symbol is shorter than TaskCnt",
                app_info.size as u32,
            ));
        }
        self.app_info = Some(app_info);
        Ok(self)
    }

    /// Set the lower bound of the PLC `_TaskInfo` array. TwinCAT's standard
    /// symbol uses one, which is the default.
    #[must_use]
    pub fn with_first_index(mut self, first_index: i32) -> Self {
        self.first_index = first_index;
        self
    }

    /// Read and decode the configured task-info symbol from `device`.
    pub fn read(&self, device: Device<'_>) -> Result<PlcTaskInfoSnapshot> {
        let mut bytes = vec![0; self.task_info.size];
        device.read_exact(self.task_info.index_group, self.task_info.index_offset, &mut bytes)?;
        let tasks = self.decode_tasks(&bytes)?;
        let reported_task_count = self.app_info.map(|location| match read_app_task_count(device, location) {
            Ok(count) => ReportedTaskCount::Available(count),
            Err(error) => ReportedTaskCount::Unavailable(error),
        });
        Ok(PlcTaskInfoSnapshot { reported_task_count, tasks })
    }

    fn decode_tasks(&self, bytes: &[u8]) -> Result<Vec<PlcTaskSystemInfo>> {
        if bytes.len() != self.task_info.size {
            return Err(Error::Reply(
                "decoding task info",
                "unexpected task-info read length",
                bytes.len() as u32,
            ));
        }
        let mut tasks = Vec::new();
        for (slot, entry) in bytes.chunks_exact(self.layout.entry_size).enumerate() {
            let task = decode_task(
                entry,
                self.first_index.checked_add(slot as i32).ok_or(Error::Reply(
                    "decoding task info",
                    "task array index overflow",
                    slot as u32,
                ))?,
                self.layout.task_name_offset,
            )?;
            if !task.cycle_time.is_zero() {
                tasks.push(task);
            }
        }
        Ok(tasks)
    }
}

fn read_app_task_count(device: Device<'_>, location: TaskInfoLocation) -> Result<u32> {
    let mut bytes = [0; APP_INFO_TASK_COUNT_SIZE];
    device.read_exact(location.index_group, location.index_offset, &mut bytes)?;
    Ok(u32::from_le_bytes(
        bytes[APP_INFO_TASK_COUNT_OFFSET..].try_into().expect("four bytes"),
    ))
}

fn decode_task(entry: &[u8], index: i32, task_name_offset: Option<usize>) -> Result<PlcTaskSystemInfo> {
    if entry.len() < TASK_PREFIX_SIZE {
        return Err(Error::Reply(
            "decoding task info",
            "task entry is shorter than documented prefix",
            entry.len() as u32,
        ));
    }
    let value_u16 = |offset| u16::from_le_bytes(entry[offset..offset + 2].try_into().expect("field length"));
    let value_u32 = |offset| u32::from_le_bytes(entry[offset..offset + 4].try_into().expect("field length"));
    let name = if let Some(offset) = task_name_offset {
        let name_bytes = &entry[offset..offset + TASK_NAME_SIZE];
        let end = name_bytes.iter().position(|byte| *byte == 0).unwrap_or(name_bytes.len());
        Some(String::from_utf8_lossy(&name_bytes[..end]).into_owned())
    } else {
        None
    };
    Ok(PlcTaskSystemInfo {
        index,
        object_id: value_u32(0),
        cycle_time: Duration::from_nanos(u64::from(value_u32(4)) * TICK_NANOS),
        priority: value_u16(8),
        ads_port: value_u16(10),
        cycle_count: value_u32(12),
        dc_task_time: i64::from_le_bytes(entry[16..24].try_into().expect("field length")),
        last_exec_time: Duration::from_nanos(u64::from(value_u32(24)) * TICK_NANOS),
        first_cycle: entry[28] != 0,
        cycle_time_exceeded: entry[29] != 0,
        in_call_after_output_update: entry[30] != 0,
        rt_violation: entry[31] != 0,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TASK_NAME_OFFSET: usize = 32;

    fn location(size: usize) -> TaskInfoLocation {
        TaskInfoLocation { index_group: 0x4020, index_offset: 100, size }
    }

    fn entry(size: usize, cycle_ticks: u32) -> Vec<u8> {
        let mut bytes = vec![0; size];
        bytes[0..4].copy_from_slice(&0x0101_0010_u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&cycle_ticks.to_le_bytes());
        bytes[8..10].copy_from_slice(&20_u16.to_le_bytes());
        bytes[10..12].copy_from_slice(&851_u16.to_le_bytes());
        bytes[12..16].copy_from_slice(&42_u32.to_le_bytes());
        bytes[16..24].copy_from_slice(&1234_i64.to_le_bytes());
        bytes[24..28].copy_from_slice(&25_000_u32.to_le_bytes());
        bytes
    }

    #[test]
    fn documented_prefix_decodes() {
        let layout = TaskInfoLayout::new(32).expect("layout");
        let reader = TaskInfoReader::new(location(32), layout).expect("reader");
        let tasks = reader.decode_tasks(&entry(32, 100_000)).expect("task");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].index, 1);
        assert_eq!(tasks[0].cycle_time, Duration::from_millis(10));
        assert_eq!(tasks[0].last_exec_time, Duration::from_micros(2500));
        assert_eq!(tasks[0].name, None);
    }

    #[test]
    fn documented_name_is_decoded_without_creating_extra_tasks() {
        let layout = TaskInfoLayout::new(96)
            .expect("layout")
            .with_task_name(TASK_NAME_OFFSET)
            .expect("name field");
        let reader = TaskInfoReader::new(location(96), layout).expect("reader");
        let mut bytes = entry(96, 100_000);
        bytes[32..41].copy_from_slice(b"FastTask\0");
        let tasks = reader.decode_tasks(&bytes).expect("task");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name.as_deref(), Some("FastTask"));
    }

    #[test]
    fn layout_requires_metadata_element_size() {
        let layout = TaskInfoLayout::new(32).expect("layout");
        assert!(TaskInfoReader::new(location(96), layout).is_ok());
        assert!(TaskInfoReader::new(location(95), layout).is_err());
        assert!(TaskInfoLayout::new(31).is_err());
    }

    #[test]
    fn empty_slots_keep_their_original_index() {
        let layout = TaskInfoLayout::new(32).expect("layout");
        let reader = TaskInfoReader::new(location(96), layout).expect("reader");
        let mut bytes = entry(32, 100_000);
        bytes.extend_from_slice(&entry(32, 0));
        bytes.extend_from_slice(&entry(32, 10_000));
        let tasks = reader.decode_tasks(&bytes).expect("tasks");
        assert_eq!(tasks.iter().map(|task| task.index).collect::<Vec<_>>(), vec![1, 3]);
    }
}
