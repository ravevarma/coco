#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing, clippy::arithmetic_side_effects)]

extern crate std;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::vec::Vec;

use super::*;
use coco_abi::syscall::rights::{ALL, DUPLICATE, READ, TRANSFER, WRITE};

#[test]
fn insert_get_remove() {
    let mut t = HandleTable::new(4);
    let h = t.insert("chan", READ | WRITE).unwrap();
    assert_eq!(t.get(h, READ).unwrap(), (&"chan", READ | WRITE));
    assert_eq!(t.get(h, TRANSFER).unwrap_err(), Error::AccessDenied);
    assert_eq!(t.remove(h).unwrap(), ("chan", READ | WRITE));
    assert_eq!(t.get(h, 0).unwrap_err(), Error::BadHandle, "closed handle rejected");
    assert_eq!(t.remove(h).unwrap_err(), Error::BadHandle, "double close rejected");
    assert!(t.is_empty());
}

#[test]
fn stale_handle_after_slot_reuse() {
    let mut t = HandleTable::new(4);
    let old = t.insert(1, ALL).unwrap();
    t.remove(old).unwrap();
    let new = t.insert(2, ALL).unwrap();
    assert_ne!(old, new, "reused slot gets a new generation");
    assert_eq!(handle::decode(old).unwrap().0, handle::decode(new).unwrap().0, "same slot");
    assert_eq!(t.get(old, 0).unwrap_err(), Error::BadHandle);
    assert_eq!(*t.get(new, 0).unwrap().0, 2);
}

#[test]
fn forged_and_invalid_values() {
    let mut t = HandleTable::new(4);
    let h = t.insert(1, READ).unwrap();
    for v in [0, 1 << 32, h + 1, h ^ (1 << 40), u64::MAX, 0xFFFF_FFFF] {
        if v != h {
            assert_eq!(t.get(v, 0).unwrap_err(), Error::BadHandle, "{v:#x}");
        }
    }
    assert_eq!(t.insert(5, 1 << 9).unwrap_err().0, Error::InvalidArgument);
}

#[test]
fn capacity_enforced() {
    let mut t = HandleTable::new(3);
    for i in 0..3 {
        t.insert(i, READ).unwrap();
    }
    let (e, obj) = t.insert(9, READ).unwrap_err();
    assert_eq!((e, obj), (Error::NoResources, 9), "object handed back");
    assert_eq!(t.available(), 0);
}

#[test]
fn duplicate_only_attenuates() {
    let mut t = HandleTable::new(8);
    let full = t.insert(7, READ | WRITE | DUPLICATE).unwrap();
    let ro = t.duplicate(full, READ).unwrap();
    assert_eq!(t.get(ro, 0).unwrap().1, READ);
    assert_eq!(t.duplicate(full, READ | TRANSFER).unwrap_err(), Error::AccessDenied, "cannot add TRANSFER");
    assert_eq!(t.duplicate(ro, READ).unwrap_err(), Error::AccessDenied, "ro lacks DUPLICATE");
    assert_eq!(t.duplicate(full, 1 << 20).unwrap_err(), Error::InvalidArgument);
    let none = t.duplicate(full, 0).unwrap();
    assert_eq!(t.get(none, READ).unwrap_err(), Error::AccessDenied);
}

#[test]
fn drain_releases_everything() {
    let obj = Rc::new(());
    let mut t = HandleTable::new(16);
    let hs: Vec<u64> = (0..10).map(|_| t.insert(Rc::clone(&obj), ALL).unwrap()).collect();
    assert_eq!(Rc::strong_count(&obj), 11);
    let drained = t.drain();
    assert_eq!(drained.len(), 10);
    drop(drained);
    assert_eq!(Rc::strong_count(&obj), 1);
    for h in hs {
        assert_eq!(t.get(h, 0).unwrap_err(), Error::BadHandle);
    }
    assert!(t.is_empty());
    t.insert(Rc::clone(&obj), ALL).unwrap();
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

/// Model-based test: random insert/remove/duplicate/get with live and stale
/// handles, checked against a map; objects are reference-counted so leaks
/// or double drops are detected.
#[test]
fn random_ops_match_model() {
    let token = Rc::new(());
    let mut t: HandleTable<(u64, Rc<()>)> = HandleTable::new(64);
    let mut model: BTreeMap<u64, (u64, u64)> = BTreeMap::new();
    let mut dead: Vec<u64> = Vec::new();
    let mut rng = Rng(0xCAB);
    for step in 0..if cfg!(miri) { 2_000 } else { 100_000 } {
        match rng.next() % 5 {
            0 | 1 => {
                let r = rng.next() & ALL;
                match t.insert((step, Rc::clone(&token)), r) {
                    Ok(h) => {
                        assert!(model.insert(h, (step, r)).is_none(), "handle value reused while live");
                    }
                    Err((e, _)) => {
                        assert_eq!(e, Error::NoResources);
                        assert_eq!(model.len(), 64);
                    }
                }
            }
            2 if !model.is_empty() => {
                let h = *model.keys().nth((rng.next() % model.len() as u64) as usize).unwrap();
                let (obj, r) = t.remove(h).unwrap();
                assert_eq!((obj.0, r), model.remove(&h).unwrap());
                dead.push(h);
            }
            3 if !model.is_empty() => {
                let h = *model.keys().nth((rng.next() % model.len() as u64) as usize).unwrap();
                let want = rng.next() & ALL;
                let (id, held) = model[&h];
                match t.duplicate(h, want) {
                    Ok(d) => {
                        assert!(held & DUPLICATE != 0 && want & !held == 0);
                        model.insert(d, (id, want));
                    }
                    Err(Error::AccessDenied) => assert!(held & DUPLICATE == 0 || want & !held != 0),
                    Err(Error::NoResources) => assert_eq!(model.len(), 64),
                    Err(e) => panic!("unexpected {e:?}"),
                }
            }
            _ => {
                if let Some(&h) = dead.get((rng.next() % (dead.len() as u64 + 1)) as usize)
                    && !model.contains_key(&h)
                {
                    assert_eq!(t.get(h, 0).unwrap_err(), Error::BadHandle, "stale handle accepted");
                }
            }
        }
        assert_eq!(t.len(), model.len());
        assert_eq!(Rc::strong_count(&token), 1 + model.len(), "object leaked or double-dropped");
    }
    drop(t.drain());
    assert_eq!(Rc::strong_count(&token), 1);
}
