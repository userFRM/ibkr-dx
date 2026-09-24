//! A table indexed by instrument slot that grows without moving what it holds.

use std::sync::OnceLock;

use crate::types::{InstrumentId, MAX_INSTRUMENTS};

/// How many buckets the directory holds: enough that every slot the id type
/// can name has a place.
const BUCKETS: usize = 21;

/// A table indexed by instrument slot, read by callers without a lock while
/// the engine writes it.
///
/// Slots are handed out for as many contracts as a session holds, so the
/// table has to grow. A reader holds a reference into it while the engine
/// writes, so growing must never move what is already there, which a vector
/// that reallocates would do. The table is a fixed directory of buckets
/// instead: the first `MAX_INSTRUMENTS` long and each after it twice the one
/// before, each built the first time a slot in it is written, and never moved.
// ponytail: a bucket once built is kept for the session; a session that once
// held a hundred thousand contracts keeps their room. Free buckets on a
// session's end if that ever matters.
pub(crate) struct SlotTable<T> {
    buckets: [OnceLock<Box<[T]>>; BUCKETS],
    blank: fn() -> T,
}

impl<T> SlotTable<T> {
    /// A table with its first bucket built, each entry made by `blank`.
    pub(crate) fn new(blank: fn() -> T) -> Self {
        let table = Self { buckets: std::array::from_fn(|_| OnceLock::new()), blank };
        table.build(0);
        table
    }

    /// Which bucket holds a slot, and where in it.
    fn locate(id: InstrumentId) -> (usize, usize) {
        let from_one = id as usize / MAX_INSTRUMENTS + 1;
        let bucket = (usize::BITS - 1 - from_one.leading_zeros()) as usize;
        (bucket, id as usize - MAX_INSTRUMENTS * ((1 << bucket) - 1))
    }

    fn build(&self, bucket: usize) -> &[T] {
        self.buckets[bucket]
            .get_or_init(|| (0..MAX_INSTRUMENTS << bucket).map(|_| (self.blank)()).collect())
    }

    /// The entry for a slot, where the table has been written that far.
    pub(crate) fn get(&self, id: InstrumentId) -> Option<&T> {
        let (bucket, at) = Self::locate(id);
        self.buckets.get(bucket)?.get()?.get(at)
    }

    /// The entry for a slot, the table grown to hold it first where nothing
    /// has been written that far yet.
    pub(crate) fn get_or_grow(&self, id: InstrumentId) -> &T {
        let (bucket, at) = Self::locate(id);
        &self.build(bucket)[at]
    }

    /// Every entry the table holds. A bucket nothing has been written to is
    /// not built, and holds nothing to visit.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.buckets.iter().filter_map(OnceLock::get).flat_map(|bucket| bucket.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A slot past the first bucket is written, read back from another
    /// thread, and leaves every slot before it where it was.
    #[test]
    fn a_slot_past_the_first_bucket_is_held_and_nothing_moves() {
        let table = std::sync::Arc::new(SlotTable::new(|| AtomicU64::new(0)));
        let first = table.get(7).unwrap() as *const AtomicU64;
        let far = (MAX_INSTRUMENTS * 5) as InstrumentId;
        assert!(table.get(far).is_none(), "nothing written that far");
        table.get_or_grow(far).store(42, Ordering::Relaxed);
        let reader = std::sync::Arc::clone(&table);
        let read = std::thread::spawn(move || reader.get(far).unwrap().load(Ordering::Relaxed));
        assert_eq!(read.join().unwrap(), 42);
        assert_eq!(
            table.get(7).unwrap() as *const AtomicU64,
            first,
            "the first bucket did not move"
        );
        assert_eq!(
            table.iter().count(),
            MAX_INSTRUMENTS * 5,
            "the first bucket and the one written to, and not the one between",
        );
    }

    /// Every slot the id type can name has a place.
    #[test]
    fn every_slot_the_id_can_name_has_a_place() {
        for id in [0, 4095, 4096, 12287, 12288, InstrumentId::MAX] {
            let (bucket, at) = SlotTable::<u8>::locate(id);
            assert!(bucket < BUCKETS && at < MAX_INSTRUMENTS << bucket, "{id}: {bucket} {at}");
        }
        assert_eq!(SlotTable::<u8>::locate(4096), (1, 0));
        assert_eq!(SlotTable::<u8>::locate(12288), (2, 0));
    }
}
