use itertools::Itertools;

use crate::commands::WriteReadRequest;

pub fn fixup_write_read_return_buffers(requests: &mut [WriteReadRequest]) {
    // Calculate the initial (using buffer sizes) and actual (using result
    // sizes) offsets of each request.
    let offsets = requests
        .iter()
        .scan((0, 0), |(init_cum, act_cum), req| {
            let (init, act) = (req.rbuf.len(), req.res.length.get() as usize);
            let current = Some((*init_cum, *act_cum, init, act));
            assert!(init >= act);
            *init_cum += init;
            *act_cum += act;
            current
        })
        .collect_vec();

    // Go through the buffers in reverse order.
    for i in (0..requests.len()).rev() {
        let (my_initial, my_actual, _, mut size) = offsets[i];
        if size == 0 {
            continue;
        }
        if my_initial == my_actual {
            // Offsets match, no further action required since all
            // previous buffers must be of full length too.
            break;
        }

        // Check in which buffer our last byte is.
        let mut j = offsets[..i + 1]
            .iter()
            .rposition(|r| r.0 < my_actual + size)
            .expect("index must be somewhere");
        let mut j_end = my_actual + size - offsets[j].0;

        // Copy the required number of bytes from every buffer from j up to i.
        loop {
            let n = j_end.min(size);
            size -= n;
            if i == j {
                requests[i].rbuf.copy_within(j_end - n..j_end, size);
            } else {
                let (first, second) = requests.split_at_mut(i);
                second[0].rbuf[size..][..n].copy_from_slice(&first[j].rbuf[j_end - n..j_end]);
            }
            if size == 0 {
                break;
            }
            j -= 1;
            j_end = offsets[j].2;
        }
    }
}

#[test]
fn test_fixup_buffers() {
    let mut buf0 = *b"12345678AB";
    let mut buf1 = *b"CDEFabc";
    let mut buf2 = *b"dxyUVW";
    let mut buf3 = *b"XYZY";
    let mut buf4 = *b"XW----";
    let mut buf5 = *b"-------------";
    let reqs = &mut [
        WriteReadRequest::new(0, 0, &[], &mut buf0),
        WriteReadRequest::new(0, 0, &[], &mut buf1),
        WriteReadRequest::new(0, 0, &[], &mut buf2),
        WriteReadRequest::new(0, 0, &[], &mut buf3),
        WriteReadRequest::new(0, 0, &[], &mut buf4),
        WriteReadRequest::new(0, 0, &[], &mut buf5),
    ];
    reqs[0].res.length.set(8);
    reqs[1].res.length.set(6);
    reqs[2].res.length.set(0);
    reqs[3].res.length.set(4);
    reqs[4].res.length.set(2);
    reqs[5].res.length.set(9);

    fixup_write_read_return_buffers(reqs);

    assert!(&reqs[5].rbuf[..9] == b"UVWXYZYXW");
    assert!(&reqs[4].rbuf[..2] == b"xy");
    assert!(&reqs[3].rbuf[..4] == b"abcd");
    assert!(&reqs[1].rbuf[..6] == b"ABCDEF");
    assert!(&reqs[0].rbuf[..8] == b"12345678");
}
