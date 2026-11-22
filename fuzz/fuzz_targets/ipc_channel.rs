//! Fuzzes the channel state machine with arbitrary operation sequences.
#![no_main]
use coco_ipc::{Channel, Error};
use libfuzzer_sys::fuzz_target;
use std::rc::Rc;

fuzz_target!(|data: &[u8]| {
    let token = Rc::new(());
    let mut c: Channel<Rc<()>> = Channel::new();
    let mut refs = [1usize, 1];
    let mut buried = 0usize;
    for chunk in data.chunks_exact(3) {
        let side = usize::from(chunk[1] & 1);
        match chunk[0] % 7 {
            0 | 1 => {
                let len = usize::from(chunk[2]) * 2;
                let hs = (0..usize::from(chunk[2] % 6)).map(|_| Rc::clone(&token)).collect();
                if let Err((e, back)) = c.send(side, &vec![0u8; len], hs) {
                    assert!(matches!(e, Error::TooLarge | Error::PeerClosed | Error::QueueFull));
                    drop(back);
                }
            }
            2 | 3 => match c.recv(side) {
                Ok(m) => drop(m),
                Err(e) => assert!(matches!(e, Error::WouldBlock | Error::PeerClosed)),
            },
            4 if refs[side] > 0 => {
                c.opened(side);
                refs[side] += 1;
            }
            5 if refs[side] > 0 => {
                refs[side] -= 1;
                if let Some((drained, _)) = c.closed(side) {
                    assert_eq!(refs[side], 0);
                    buried += drained.len();
                }
            }
            _ => {
                let _ = c.add_waiter(side, u64::from(chunk[2]));
            }
        }
        let in_flight = c.in_flight().count();
        assert_eq!(Rc::strong_count(&token), 1 + in_flight, "handle leaked or double-dropped");
        assert!(c.queued(0) <= 64 && c.queued(1) <= 64);
    }
    let _ = buried;
});
