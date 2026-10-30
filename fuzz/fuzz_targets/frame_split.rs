#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let (notifications, _) = ads::test_util::split_frames(data);
    for notification in &notifications {
        notification.samples().for_each(|sample| {
            std::hint::black_box(sample);
        });
    }
});
