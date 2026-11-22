#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

extern crate std;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec;
use std::vec::Vec;

use super::*;

#[test]
fn send_recv_both_directions() {
    let mut c: Channel<u32> = Channel::new();
    c.send(0, b"ping", vec![7]).unwrap();
    c.send(1, b"pong", vec![]).unwrap();
    assert!(c.readable(1) && c.readable(0));
    assert_eq!(c.recv(1).unwrap(), Message { data: b"ping".to_vec(), handles: vec![7] });
    assert_eq!(c.recv(0).unwrap().data, b"pong");
    assert_eq!(c.recv(0).unwrap_err(), Error::WouldBlock);
    assert!(!c.readable(0));
}

#[test]
fn limits_enforced_and_handles_returned() {
    let mut c: Channel<u32> = Channel::new();
    let big = vec![0u8; MAX_MESSAGE_BYTES as usize + 1];
    let (e, hs) = c.send(0, &big, vec![1, 2]).unwrap_err();
    assert_eq!((e, hs), (Error::TooLarge, vec![1, 2]));
    let (e, hs) = c.send(0, b"", vec![0; MAX_MESSAGE_HANDLES as usize + 1]).unwrap_err();
    assert_eq!((e, hs.len()), (Error::TooLarge, MAX_MESSAGE_HANDLES as usize + 1));
    for _ in 0..MAX_QUEUED_MESSAGES {
        c.send(0, b"x", vec![]).unwrap();
    }
    let (e, hs) = c.send(0, b"x", vec![9]).unwrap_err();
    assert_eq!((e, hs), (Error::QueueFull, vec![9]));
    assert_eq!(c.queued(1), MAX_QUEUED_MESSAGES);
    c.send(0, &vec![1u8; MAX_MESSAGE_BYTES as usize][..], vec![]).unwrap_err();
    c.recv(1).unwrap();
    c.send(0, &vec![1u8; MAX_MESSAGE_BYTES as usize][..], vec![]).unwrap();
}

#[test]
fn peer_closed_semantics() {
    let mut c: Channel<u32> = Channel::new();
    c.send(0, b"last words", vec![]).unwrap();
    let (drained, _) = c.closed(0).unwrap();
    assert!(drained.is_empty(), "side 0 had nothing inbound");
    // Queued data is still delivered, then PeerClosed.
    assert!(c.readable(1));
    assert_eq!(c.recv(1).unwrap().data, b"last words");
    assert_eq!(c.recv(1).unwrap_err(), Error::PeerClosed);
    assert!(c.readable(1), "closed peer counts as readable");
    let (e, hs) = c.send(1, b"hello?", vec![3]).unwrap_err();
    assert_eq!((e, hs), (Error::PeerClosed, vec![3]));
}

#[test]
fn reference_counting_and_drain() {
    let token = Rc::new(());
    let mut c: Channel<Rc<()>> = Channel::new();
    c.opened(1);
    c.send(0, b"a", vec![Rc::clone(&token), Rc::clone(&token)]).unwrap();
    assert_eq!(Rc::strong_count(&token), 3);
    assert!(c.closed(1).is_none(), "one reference to side 1 remains");
    let (drained, _) = c.closed(1).unwrap();
    assert_eq!(drained.len(), 1);
    drop(drained);
    assert_eq!(Rc::strong_count(&token), 1, "in-flight handles released on close");
    assert!(!c.is_open(1));
}

#[test]
fn waiters_registered_and_returned() {
    let mut c: Channel<u32> = Channel::new();
    for tid in 1..=MAX_WAITERS as u64 {
        c.add_waiter(1, tid).unwrap();
    }
    c.add_waiter(1, 1).unwrap(); // duplicate is a no-op
    assert_eq!(c.add_waiter(1, 99).unwrap_err(), Error::NoResources);
    c.remove_waiter(1, 2);
    let wake = c.send(0, b"x", vec![]).unwrap();
    let woken: Vec<u64> = wake.iter().flatten().copied().collect();
    assert_eq!(woken, vec![1, 3, 4]);
    assert!(c.send(0, b"y", vec![]).unwrap().iter().all(Option::is_none), "waiters consumed");
    c.add_waiter(0, 42).unwrap();
    let (_, wake) = c.closed(1).unwrap();
    assert_eq!(wake[0], Some(42), "peer close wakes the other side");
}

#[test]
fn in_flight_lists_queued_handles() {
    let mut c: Channel<u32> = Channel::new();
    c.send(0, b"a", vec![1, 2]).unwrap();
    c.send(1, b"b", vec![3]).unwrap();
    let mut seen: Vec<u32> = c.in_flight().copied().collect();
    seen.sort_unstable();
    assert_eq!(seen, vec![1, 2, 3]);
    c.recv(1).unwrap();
    assert_eq!(c.in_flight().copied().collect::<Vec<_>>(), vec![3]);
}

#[test]
fn unrecv_restores_order() {
    let mut c: Channel<u32> = Channel::new();
    c.send(0, b"1", vec![]).unwrap();
    c.send(0, b"2", vec![]).unwrap();
    let m = c.recv(1).unwrap();
    c.unrecv(1, m);
    assert_eq!(c.recv(1).unwrap().data, b"1");
    assert_eq!(c.recv(1).unwrap().data, b"2");
}

#[test]
fn out_of_range_side_is_masked() {
    let mut c: Channel<u32> = Channel::new();
    c.send(2, b"x", vec![]).unwrap(); // 2 & 1 == 0
    assert_eq!(c.recv(1).unwrap().data, b"x");
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Model test: random sends, receives and closes; FIFO order and contents
/// match a model, and every handle is released exactly once.
#[test]
fn random_ops_match_model() {
    let token = Rc::new(());
    let mut rng = Rng(0x1BC);
    for _ in 0..if cfg!(miri) { 20 } else { 400 } {
        let mut c: Channel<Rc<()>> = Channel::new();
        let mut model: [VecDeque<(Vec<u8>, usize)>; 2] = [VecDeque::new(), VecDeque::new()];
        let mut open = [true, true];
        for _ in 0..200 {
            let s = (rng.next() % 2) as usize;
            let to = 1 - s;
            match rng.next() % 10 {
                0..=4 if open[s] => {
                    let data: Vec<u8> = (0..rng.next() % 300).map(|i| i as u8).collect();
                    let nh = (rng.next() % 6) as usize;
                    let hs: Vec<Rc<()>> = (0..nh).map(|_| Rc::clone(&token)).collect();
                    match c.send(s, &data, hs) {
                        Ok(_) => model[to].push_back((data, nh)),
                        Err((e, back)) => {
                            assert_eq!(back.len(), nh, "handles returned on failure");
                            let expect = if data.len() > 256 || nh > 4 {
                                Error::TooLarge
                            } else if !open[to] {
                                Error::PeerClosed
                            } else {
                                Error::QueueFull
                            };
                            assert_eq!(e, expect);
                        }
                    }
                }
                5..=8 if open[s] => match c.recv(s) {
                    Ok(m) => {
                        let (d, nh) = model[s].pop_front().unwrap();
                        assert_eq!((m.data, m.handles.len()), (d, nh));
                    }
                    Err(e) => {
                        assert!(model[s].is_empty());
                        assert_eq!(e, if open[to] { Error::WouldBlock } else { Error::PeerClosed });
                    }
                },
                9 if open[s] => {
                    let (drained, _) = c.closed(s).unwrap();
                    assert_eq!(drained.len(), model[s].len());
                    model[s].clear();
                    open[s] = false;
                }
                _ => {}
            }
            let queued_handles: usize = model.iter().flatten().map(|(_, n)| n).sum();
            assert_eq!(Rc::strong_count(&token), 1 + queued_handles, "handle leaked or double-dropped");
        }
        drop(c);
        assert_eq!(Rc::strong_count(&token), 1);
    }
}
