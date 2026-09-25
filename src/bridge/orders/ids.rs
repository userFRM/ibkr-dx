//! Order numbers retained across sessions sharing the same settings.

use super::{MAX_ORDER_ID, OrderState};
use crate::error_codes::Refusal;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

type Counters = BTreeMap<String, BTreeMap<i32, u64>>;

pub(super) struct SavedIds {
    path: PathBuf,
    /// What the account's counters are filed under.
    key: String,
    client: i32,
    /// The next id as this session last read or wrote it. The saved counter
    /// only rises, so a number below it needs no write.
    next: u64,
}

/// What an account's counters are filed under: the SHA-256 digest of the
/// account, in lowercase hex, so the file does not name the account.
fn key(account: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(account.as_bytes()))
}

/// File what a file written before the keys were digests holds under each
/// account's digest, keeping the higher counter where both are held. Answers
/// whether there was any.
fn migrate(counters: &mut Counters) -> bool {
    let named: Vec<String> = counters.keys()
        .filter(|held| !(held.len() == 64 && held.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))))
        .cloned().collect();
    for account in &named {
        let ids = counters.remove(account).unwrap_or_default();
        let filed = counters.entry(key(account)).or_default();
        for (client, next) in ids {
            let held = filed.entry(client).or_insert(next);
            *held = (*held).max(next);
        }
    }
    !named.is_empty()
}

/// Every counter the file holds. Read without the lock: the file is only
/// ever replaced whole.
fn read(path: &Path) -> io::Result<Counters> {
    match File::open(path) {
        Ok(file) => Ok(serde_json::from_reader(io::BufReader::new(file))?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Counters::new()),
        Err(error) => Err(error),
    }
}

/// Created readable by its owner alone.
fn private() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
}

fn fall_back(path: &Path, error: &io::Error) {
    log::warn!(
        "cannot retain order ids in {}: {error}; this session will use memory and venue replay only",
        path.display()
    );
}

impl SavedIds {
    fn update(&mut self, change: impl FnOnce(u64) -> u64) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut lock_path = self.path.as_os_str().to_owned();
        lock_path.push(".lock");
        let lock = private().read(true).write(true).create(true).truncate(false).open(lock_path)?;
        // The lock has a stable name: locking the data file would leave another
        // process holding the old file after its replacement.
        lock.lock()?;
        let mut counters = read(&self.path)?;
        let migrated = migrate(&mut counters);
        let next = counters.entry(self.key.clone()).or_default().entry(self.client).or_insert(1);
        let raised = change(*next).max(*next);
        if raised != *next || migrated {
            *next = raised;
            let mut temporary = self.path.as_os_str().to_owned();
            temporary.push(".tmp");
            // Written under the lock alone, so anything already there is a
            // write that never finished, or not this file's: it is replaced,
            // never followed.
            let _ = std::fs::remove_file(&temporary);
            let mut file = private().write(true).create_new(true).open(&temporary)?;
            serde_json::to_writer(&mut file, &counters)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            // The counter is in place once renamed; a directory the
            // filesystem cannot sync still holds it.
            #[cfg(unix)]
            if let Err(error) = File::open(parent).and_then(|dir| dir.sync_all()) {
                log::debug!("cannot sync {}: {error}", parent.display());
            }
        }
        self.next = raised;
        Ok(())
    }
}

impl OrderState {
    /// Read the counter before the session can announce its first order id,
    /// and return the next id it holds.
    ///
    /// A file that cannot be read leaves the session on memory and venue
    /// replay; one that can be read floors the session even where it cannot
    /// be written. A file that still names accounts is rewritten under their
    /// digests before the session goes on.
    pub(crate) fn open_order_ids(&self, path: Option<&Path>, account: &str) -> u64 {
        let client = self.api_client_id();
        let mut next = 1;
        let store = path.and_then(|path| match read(path) {
            Ok(mut counters) => {
                let migrated = migrate(&mut counters);
                let key = key(account);
                let saved = counters.get(&key).and_then(|ids| ids.get(&client)).copied();
                let mut store = SavedIds { next: saved.unwrap_or(1), path: path.to_owned(), key, client };
                let written = if migrated { store.update(|next| next) } else { Ok(()) };
                next = store.next.max(1);
                written.map_err(|error| fall_back(path, &error)).ok().map(|()| store)
            }
            Err(error) => {
                fall_back(path, &error);
                None
            }
        });
        *self.saved_ids.lock().unwrap() = store;
        self.saved_before.store(next - 1, Ordering::Release);
        if next > 1 {
            self.note_the_venue_named(next - 1);
        }
        next
    }

    /// The highest id saved before this session opened. A new order at or
    /// below it repeats a number already used.
    pub(crate) fn saved_before(&self) -> u64 {
        self.saved_before.load(Ordering::Acquire)
    }

    fn with_saved_ids<T>(&self, change: impl FnOnce(u64) -> (u64, T)) -> T {
        let mut saved = self.saved_ids.lock().unwrap();
        let mut change = Some(change);
        let mut result = None;
        if let Some(store) = saved.as_mut() {
            let updated = store.update(|next| {
                let (next, value) = change.take().unwrap()(next);
                result = Some(value);
                next
            });
            if let Err(error) = updated {
                fall_back(&store.path, &error);
                *saved = None;
            }
        }
        result.unwrap_or_else(|| change.take().unwrap()(1).1)
    }

    /// Save the next id the session's numbers have reached, on the caller's
    /// thread, before its command reaches the engine. A number the counter is
    /// already past is not written again.
    pub(crate) fn save_order_ids(&self, next: u64) {
        if self.saved_ids.lock().unwrap().as_ref().is_some_and(|store| store.next < next) {
            self.with_saved_ids(|saved| (saved.max(next), ()));
        }
    }

    /// Reserve a consecutive run before handing any of its numbers out.
    pub(crate) fn reserve_order_ids(
        &self,
        allocator: &AtomicU64,
        count: u64,
    ) -> Result<u64, Refusal> {
        self.with_saved_ids(|saved| {
            match reserve_order_ids(
                allocator,
                count,
                saved.max(self.working_id_watermark().saturating_add(1)),
            ) {
                Ok(first) => (first + count, Ok(first)),
                Err(why) => (saved, Err(why)),
            }
        })
    }

    /// Number an order the engine places on its own, in memory: the engine's
    /// loop does not wait on the file, and the caller's next save or
    /// reservation, counted from the same allocator, carries the number there.
    pub(crate) fn number_in_memory(&self, allocator: &AtomicU64) -> Result<u64, Refusal> {
        reserve_order_ids(allocator, 1, self.working_id_watermark().saturating_add(1))
    }
}

pub(crate) fn reserve_order_ids(
    allocator: &AtomicU64,
    count: u64,
    floor: u64,
) -> Result<u64, Refusal> {
    let mut held = allocator.load(Ordering::Acquire);
    loop {
        let first = held.max(floor);
        let last = first.checked_add(count - 1).filter(|last| *last <= MAX_ORDER_ID)
            .ok_or_else(|| Refusal::validation(format!(
                "this account has no run of {count} order ids left: the ids in use reach {first}, and an order above {MAX_ORDER_ID} cannot be named back by the venue's reports",
            )))?;
        match allocator.compare_exchange_weak(held, last + 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                super::say_if_past_a_request_id(last);
                return Ok(first);
            }
            Err(seen) => held = seen,
        }
    }
}
