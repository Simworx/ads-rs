//! TwinCAT 3 PLC task telemetry.
//!
//! [`TaskInfoReader`] is a reusable description of the task-info symbols and
//! their validated element layout. Construct it while resolving symbols, retain
//! it for repeated samples, and recreate it when that symbol metadata changes.

use std::convert::TryInto;
use std::time::Duration;

use crate::symbol::Type;
use crate::{AmsAddr, Device, Error, Result};

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
/// constructor validates the documented fields' offsets, sizes, and ADS base
/// types. Task names are decoded only when their `STRING(63)` field is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskInfoLayout {
    entry_size: usize,
    task_name_offset: Option<usize>,
}

impl TaskInfoLayout {
    /// Validate and create a layout from the `_TaskInfo` array element type.
    ///
    /// The caller must resolve aliases before passing this type: fields must
    /// retain their ADS primitive base types in the supplied metadata.
    pub fn from_entry_type(entry: &Type) -> Result<Self> {
        if entry.size < TASK_PREFIX_SIZE {
            return Err(Error::TaskInfoLayout("task entry is shorter than documented prefix"));
        }

        let required = [
            ("ObjId", 0, 4, 19),
            ("CycleTime", 4, 4, 19),
            ("Priority", 8, 2, 18),
            ("AdsPort", 10, 2, 18),
            ("CycleCount", 12, 4, 19),
            ("DcTaskTime", 16, 8, 20),
            ("LastExecTime", 24, 4, 19),
            ("FirstCycle", 28, 1, 33),
            ("CycleTimeExceeded", 29, 1, 33),
            ("InCallAfterOutputUpdate", 30, 1, 33),
            ("RTViolation", 31, 1, 33),
        ];
        for (name, offset, size, base_type) in required {
            let field = entry
                .fields
                .iter()
                .find(|field| field.name == name)
                .ok_or(Error::TaskInfoLayout("a required task field is missing"))?;
            if field.offset != Some(offset)
                || field.size != size
                || field.base_type != base_type
                || !field.array.is_empty()
            {
                return Err(Error::TaskInfoLayout(
                    "a required task field has an unsupported type, size, or offset",
                ));
            }
        }

        let task_name_offset = match entry.fields.iter().find(|field| field.name == "TaskName") {
            Some(field)
                if field.offset.map_or(false, |offset| {
                    let offset = offset as usize;
                    offset >= TASK_PREFIX_SIZE
                        && offset.checked_add(TASK_NAME_SIZE).map_or(false, |end| end <= entry.size)
                })
                    && field.size == TASK_NAME_SIZE
                    && field.base_type == 30
                    && field.array.is_empty() =>
            {
                field.offset.map(|offset| offset as usize)
            }
            Some(_) => {
                return Err(Error::TaskInfoLayout("TaskName has an unsupported type, size, or offset"))
            }
            None => None,
        };
        Ok(Self { entry_size: entry.size, task_name_offset })
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
    /// Original PLC array index, using the metadata bounds supplied to
    /// [`TaskInfoReader::new`].
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
    /// Signed distributed-clock task time in nanoseconds.
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
///
/// The number of array entries is not the number of monitored tasks: unused
/// slots are retained. Consumers determine coverage using their own validated
/// task-selection policy and the reported count; this snapshot does not infer
/// completeness from array capacity.
#[derive(Debug)]
pub struct PlcTaskInfoSnapshot {
    /// The application's reported task count, when an app-info location was
    /// supplied to the reader.
    pub reported_task_count: Option<ReportedTaskCount>,
    /// Task entries, retaining their original array indices, including entries
    /// whose configured cycle time is zero.
    pub tasks: Vec<PlcTaskSystemInfo>,
}

/// Cached, validated metadata used to read TwinCAT task telemetry.
///
/// `TaskInfoReader` does not discover symbols. Pass locations and array bounds
/// from an uploaded symbol table or another caller-owned cache, then reuse this
/// value for each sample. `symbol_generation` is owned by that cache; callers
/// must pass their current generation to every read and recreate this reader
/// after reconnecting, downloading a project, or an online change.
///
/// The cache must monitor the target's symbol version (ADS index group
/// [`crate::index::GET_SYMVERSION`]) using polling or symbol-version notifications.
/// On a version change, invalidate cached locations and advance the local
/// generation before rebuilding metadata and readers. Reconnects must also
/// advance the generation, even if the target reports the same version. If a
/// version check fails, suspend sampling until metadata validity is established.
/// A local generation alone cannot detect changes on the target, and successful
/// direct address reads are not evidence that cached metadata is still valid.
/// Version checks and value reads are separate operations: changes can race a
/// sample, so discard it when a version change is observed during sampling.
#[derive(Debug, Clone, Copy)]
pub struct TaskInfoReader {
    target: AmsAddr,
    symbol_generation: u64,
    task_info: TaskInfoLocation,
    app_info: Option<TaskInfoLocation>,
    layout: TaskInfoLayout,
    first_index: i32,
}

impl TaskInfoReader {
    /// Create a reader for one target and symbol-metadata generation.
    ///
    /// `array_bounds` are the one-dimensional `_TaskInfo` bounds from symbol
    /// metadata. They must exactly describe the supplied symbol's size.
    pub fn new(
        target: AmsAddr, symbol_generation: u64, task_info: TaskInfoLocation, array_bounds: (i32, i32),
        layout: TaskInfoLayout,
    ) -> Result<Self> {
        let (first_index, last_index) = array_bounds;
        let count = last_index
            .checked_sub(first_index)
            .filter(|count| *count >= 0)
            .and_then(|n| n.checked_add(1))
            .ok_or(Error::TaskInfoLayout("task array bounds are invalid"))? as usize;
        if task_info.size == 0
            || task_info.size > MAX_TASK_INFO_SIZE
            || count.checked_mul(layout.entry_size) != Some(task_info.size)
        {
            return Err(Error::TaskInfoLayout("task array bounds and symbol size disagree"));
        }
        Ok(Self { target, symbol_generation, task_info, app_info: None, layout, first_index })
    }

    /// Include a resolved `_AppInfo` location so [`Self::read`] also attempts to
    /// obtain `TaskCnt`. A failure there leaves task data available in the snapshot.
    pub fn with_app_info(mut self, app_info: TaskInfoLocation) -> Result<Self> {
        if app_info.size < APP_INFO_TASK_COUNT_SIZE {
            return Err(Error::TaskInfoLayout("app-info symbol is shorter than TaskCnt"));
        }
        self.app_info = Some(app_info);
        Ok(self)
    }

    /// Return the caller-owned symbol metadata generation for this reader.
    #[must_use]
    pub fn symbol_generation(&self) -> u64 {
        self.symbol_generation
    }

    /// Read and decode the configured task-info symbol from `device`.
    ///
    /// Pass the current generation from the caller's symbol cache. A mismatch
    /// returns [`Error::TaskInfoInvalidated`] before any ADS requests are sent.
    /// This performs one value read, plus one when app-info is configured; it
    /// performs no discovery or symbol-version checks. The two value reads do
    /// not form an atomic snapshot. See the reader's cache invalidation contract.
    pub fn read(&self, device: Device<'_>, current_generation: u64) -> Result<PlcTaskInfoSnapshot> {
        if current_generation != self.symbol_generation {
            return Err(Error::TaskInfoInvalidated);
        }
        if device.address() != self.target {
            return Err(Error::TaskInfoLayout("reader was used with a different ADS target"));
        }
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
            tasks.push(task);
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
    use crate::symbol::Field;

    fn location(size: usize) -> TaskInfoLocation {
        TaskInfoLocation { index_group: 0x4020, index_offset: 100, size }
    }

    fn field(name: &str, offset: u32, size: usize, base_type: u32) -> Field {
        Field {
            name: name.into(),
            typ: name.into(),
            offset: Some(offset),
            size,
            array: vec![],
            base_type,
            flags: 0,
            comment: String::new(),
            attributes: None,
        }
    }

    fn entry_type(with_name: bool) -> Type {
        let mut fields = vec![
            field("ObjId", 0, 4, 19),
            field("CycleTime", 4, 4, 19),
            field("Priority", 8, 2, 18),
            field("AdsPort", 10, 2, 18),
            field("CycleCount", 12, 4, 19),
            field("DcTaskTime", 16, 8, 20),
            field("LastExecTime", 24, 4, 19),
            field("FirstCycle", 28, 1, 33),
            field("CycleTimeExceeded", 29, 1, 33),
            field("InCallAfterOutputUpdate", 30, 1, 33),
            field("RTViolation", 31, 1, 33),
        ];
        if with_name {
            fields.push(field("TaskName", 32, 64, 30));
        }
        Type {
            name: "PlcTaskSystemInfo".into(),
            type_name: "PlcTaskSystemInfo".into(),
            comment: String::new(),
            size: if with_name { 96 } else { 32 },
            array: vec![],
            fields,
            base_type: 65,
            flags: 0,
            guid: None,
            methods: None,
            attributes: None,
            enum_info: None,
        }
    }

    fn reader(size: usize, with_name: bool) -> TaskInfoReader {
        TaskInfoReader::new(
            AmsAddr::default(),
            7,
            location(size),
            (1, 1),
            TaskInfoLayout::from_entry_type(&entry_type(with_name)).expect("layout"),
        )
        .expect("reader")
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
        let reader = reader(32, false);
        let tasks = reader.decode_tasks(&entry(32, 100_000)).expect("task");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].index, 1);
        assert_eq!(tasks[0].cycle_time, Duration::from_millis(10));
        assert_eq!(tasks[0].last_exec_time, Duration::from_micros(2500));
        assert_eq!(tasks[0].name, None);
    }

    #[test]
    fn documented_name_is_decoded_without_creating_extra_tasks() {
        let reader = reader(96, true);
        let mut bytes = entry(96, 100_000);
        bytes[32..41].copy_from_slice(b"FastTask\0");
        let tasks = reader.decode_tasks(&bytes).expect("task");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name.as_deref(), Some("FastTask"));
    }

    #[test]
    fn plc_name_at_offset_64_uses_128_byte_stride() {
        // Layout captured from Timmy_Dev's uploaded PLC metadata.
        let mut metadata = entry_type(true);
        metadata.size = 128;
        metadata.fields.last_mut().unwrap().offset = Some(64);
        let layout = TaskInfoLayout::from_entry_type(&metadata).expect("PLC layout");
        let reader = TaskInfoReader::new(AmsAddr::default(), 7, location(256), (1, 2), layout)
            .expect("reader");
        let mut first = entry(128, 10_000);
        first[32..64].fill(0xff); // The gap must not be decoded as a name or task.
        first[64..73].copy_from_slice(b"FastTask\0");
        let mut second = entry(128, 100_000);
        second[64..73].copy_from_slice(b"SlowTask\0");
        first.extend_from_slice(&second);
        let tasks = reader.decode_tasks(&first).expect("tasks");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].index, 1);
        assert_eq!(tasks[0].name.as_deref(), Some("FastTask"));
        assert_eq!(tasks[0].cycle_time, Duration::from_millis(1));
        assert_eq!(tasks[1].index, 2);
        assert_eq!(tasks[1].name.as_deref(), Some("SlowTask"));
        assert_eq!(tasks[1].cycle_time, Duration::from_millis(10));
    }

    #[test]
    fn rejects_invalid_task_name_offsets() {
        for offset in [None, Some(0), Some(31), Some(65), Some(u32::MAX)] {
            let mut metadata = entry_type(true);
            metadata.size = 128;
            metadata.fields.last_mut().unwrap().offset = offset;
            assert!(matches!(
                TaskInfoLayout::from_entry_type(&metadata),
                Err(Error::TaskInfoLayout(_))
            ));
        }
    }

    #[test]
    fn layout_requires_metadata_fields_and_array_bounds() {
        let mut invalid = entry_type(false);
        invalid.fields[0].offset = Some(4);
        assert!(TaskInfoLayout::from_entry_type(&invalid).is_err());

        let layout = TaskInfoLayout::from_entry_type(&entry_type(true)).expect("layout");
        assert!(TaskInfoReader::new(AmsAddr::default(), 7, location(96), (1, 1), layout).is_ok());
        // A 96-byte extended entry must not be treated as three 32-byte entries.
        let legacy = TaskInfoLayout::from_entry_type(&entry_type(false)).expect("legacy layout");
        assert!(TaskInfoReader::new(AmsAddr::default(), 7, location(96), (1, 1), legacy).is_err());
        assert!(TaskInfoReader::new(AmsAddr::default(), 7, location(96), (2, 1), layout).is_err());
    }

    #[test]
    fn empty_slots_keep_their_original_index() {
        let layout = TaskInfoLayout::from_entry_type(&entry_type(false)).expect("layout");
        let reader =
            TaskInfoReader::new(AmsAddr::default(), 7, location(96), (1, 3), layout).expect("reader");
        let mut bytes = entry(32, 100_000);
        bytes.extend_from_slice(&entry(32, 0));
        bytes.extend_from_slice(&entry(32, 10_000));
        let tasks = reader.decode_tasks(&bytes).expect("tasks");
        assert_eq!(tasks.iter().map(|task| task.index).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn rejects_task_name_outside_entry() {
        for size in [32, 64, 95] {
            let mut metadata = entry_type(true);
            metadata.size = size;
            assert!(matches!(
                TaskInfoLayout::from_entry_type(&metadata),
                Err(Error::TaskInfoLayout(_))
            ));
        }
    }

    #[test]
    fn truncated_data_is_a_reply_error() {
        assert!(matches!(reader(96, true).decode_tasks(&[0; 95]), Err(Error::Reply(..))));
    }

    #[test]
    fn reader_reads_values_and_preserves_count_failure() {
        let port = crate::test::config_test_server(Default::default());
        let client =
            crate::Client::new(("127.0.0.1", port), crate::Timeouts::none(), crate::Source::Auto).unwrap();
        let device = client.device(AmsAddr::new(crate::AmsNetId::new(1, 2, 3, 4, 5, 6), 851));
        let mut reader = reader(32, false);
        reader.target = device.address();
        // The fake server supports value reads here, but no type discovery.
        device.write(0x4020, 100, &entry(32, 100_000)).unwrap();
        device.write(0x4020, 204, &3u32.to_le_bytes()).unwrap();
        let app = TaskInfoLocation { index_group: 0x4020, index_offset: 200, size: 8 };
        let reader = reader.with_app_info(app).unwrap();
        for _ in 0..2 {
            let snapshot = reader.read(device, 7).unwrap();
            assert_eq!(snapshot.tasks.len(), 1);
            assert!(matches!(snapshot.reported_task_count, Some(ReportedTaskCount::Available(3))));
        }
        let reader = reader.with_app_info(TaskInfoLocation { index_group: 0xffff, ..app }).unwrap();
        let snapshot = reader.read(device, 7).unwrap();
        assert_eq!(snapshot.tasks[0].cycle_count, 42);
        assert!(matches!(
            snapshot.reported_task_count,
            Some(ReportedTaskCount::Unavailable(Error::Ads(..)))
        ));

        assert!(matches!(reader.read(device, 8), Err(Error::TaskInfoInvalidated)));
        let other = client.device(AmsAddr::new(device.address().netid(), 852));
        assert!(matches!(reader.read(other, 7), Err(Error::TaskInfoLayout(_))));

        let mut failed = reader;
        failed.task_info.index_group = 0xffff;
        assert!(matches!(failed.read(device, 7), Err(Error::Ads(..))));
    }
}
