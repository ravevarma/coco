//! Fuzzes the capability table with arbitrary operation sequences and
//! handle values (as a hostile process would supply them).
#![no_main]
use coco_capability::HandleTable;
use libfuzzer_sys::fuzz_target;
use std::rc::Rc;

fuzz_target!(|data: &[u8]| {
    let token = Rc::new(());
    let mut t: HandleTable<Rc<()>> = HandleTable::new(16);
    let mut issued: Vec<u64> = Vec::new();
    for chunk in data.chunks_exact(9) {
        let op = chunk[0] % 5;
        let raw = u64::from_le_bytes(chunk[1..9].try_into().unwrap());
        // Mix issued handles with arbitrary (forged) values.
        let h = if raw % 3 == 0 && !issued.is_empty() { issued[(raw as usize / 3) % issued.len()] } else { raw };
        match op {
            0 => {
                if let Ok(v) = t.insert(Rc::clone(&token), raw & 0xF) {
                    issued.push(v);
                }
            }
            1 => {
                let _ = t.remove(h);
            }
            2 => {
                if let Ok(v) = t.duplicate(h, raw >> 32) {
                    issued.push(v);
                }
            }
            3 => {
                if let Ok((_, r)) = t.get(h, raw & 0xF) {
                    assert_eq!(r & (raw & 0xF), raw & 0xF, "rights check bypassed");
                }
            }
            _ => drop(t.drain()),
        }
        assert!(t.len() <= 16);
        assert_eq!(Rc::strong_count(&token), 1 + t.len(), "object leaked or double-dropped");
    }
});
