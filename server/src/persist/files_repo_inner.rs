use std::collections::BTreeMap;

use ahash::AHashMap;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use super::size_class::{MIN_SIZE_CLASS, size_class_for};
use super::slot::{SLOT_PREFIX_LEN, SlotState, decode_slot, encode_slot, slot_bytes_needed};
use super::{LoadedPartition, LoadedTableAttrs, TableMetadataFileContract};

const TABLES_META_FILE: &str = "tables.meta";

/// Where a partition's slot lives: which size-class page-file and which slot
/// index inside it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SlotLocation {
    pub size_class: u32,
    pub slot_index: u64,
}

impl SlotLocation {
    fn offset(&self) -> u64 {
        self.slot_index * self.size_class as u64
    }
}

/// Runtime state of one size-class page-file.
struct ClassState {
    /// Number of slots the file currently holds (file_len / size_class).
    slot_count: u64,
    /// Indices of freed slots available for reuse. In-memory only: a freed slot
    /// is self-describing on disk (`body_len == 0`), so the list is rebuilt by
    /// the recovery scan - nothing to persist.
    free: Vec<u64>,
}

/// The in-memory bookkeeping of the files backend. Rebuilt entirely by scanning
/// the page-files on `load_all_partitions`, so nothing here needs to be a
/// crash-consistent on-disk structure besides the slots themselves.
pub struct FilesRepoInner {
    root: String,
    classes: AHashMap<u32, ClassState>,
    /// (table_name, partition_key) -> slot location.
    index: AHashMap<(String, String), SlotLocation>,
    /// table_name -> attributes (mirrored to `<root>/tables.meta`).
    tables: BTreeMap<String, TableMetadataFileContract>,
    /// Monotonic per-write counter used as the slot `version` for crash-time
    /// duplicate resolution. Seeded above the max version seen on disk during
    /// the recovery scan, so it stays monotonic across restarts and never
    /// depends on the (non-monotonic) wall clock.
    next_version: u64,
}

/// A slot picked up by the recovery scan.
struct ScannedSlot {
    key: (String, String),
    location: SlotLocation,
    version: u64,
    payload: Vec<u8>,
}

impl FilesRepoInner {
    pub async fn open(root: String, skip_errors: bool) -> Self {
        tokio::fs::create_dir_all(&root)
            .await
            .expect("files_repo: can not create root directory");

        let tables = load_tables_meta(&root, skip_errors).await;
        let classes = discover_class_files(&root).await;

        Self {
            root,
            classes,
            index: AHashMap::new(),
            tables,
            next_version: 0,
        }
    }

    fn class_path(&self, size_class: u32) -> String {
        format!("{}/{}", self.root, size_class)
    }

    // ---- reads / init ----------------------------------------------------

    pub fn get_tables(&self) -> Vec<LoadedTableAttrs> {
        self.tables
            .iter()
            .map(|(table_name, contract)| LoadedTableAttrs {
                table_name: table_name.clone(),
                attr: contract.clone().into(),
            })
            .collect()
    }

    /// Scans every page-file, rebuilds the in-memory index and free-lists, and
    /// returns every live partition's payload. After a crash mid relocation two
    /// slots may carry the same key - the higher `version` wins and the loser is
    /// zeroed on disk so it can never resurrect later.
    pub async fn load_all_partitions(&mut self, skip_errors: bool) -> Vec<LoadedPartition> {
        let class_sizes: Vec<u32> = self.classes.keys().copied().collect();

        let mut scanned: Vec<ScannedSlot> = Vec::new();
        let mut max_version: u64 = 0;

        for size_class in class_sizes {
            let slot_count = self.classes.get(&size_class).unwrap().slot_count;
            let path = self.class_path(size_class);

            // An unreadable page-file is always fatal, even with
            // SkipBrokenPartitions: unlike a corrupt slot (which the scan zeroes
            // so it can never resurrect), an unread file can not be neutralized -
            // its partitions would silently vanish for this run and its versions
            // would be missing from the `next_version` seed, so partitions
            // re-saved later would lose the dedup against the stale slots on the
            // following restart and be destroyed. NotFound is tolerated: the file
            // disappeared after discovery, so the class is simply empty.
            let file_bytes = match tokio::fs::read(&path).await {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(err) => panic!("files_repo: can not read page-file {path}: {err}"),
            };

            let slot_len = size_class as usize;

            for slot_index in 0..slot_count {
                let start = slot_index as usize * slot_len;
                let end = start + slot_len;

                if end > file_bytes.len() {
                    break;
                }

                match decode_slot(&file_bytes[start..end]) {
                    SlotState::Free => {
                        self.classes
                            .get_mut(&size_class)
                            .unwrap()
                            .free
                            .push(slot_index);
                    }
                    SlotState::Corrupt => {
                        if !skip_errors {
                            panic!("files_repo: corrupt slot {slot_index} in page-file {path}");
                        }

                        println!("files_repo: skipping corrupt slot {slot_index} in {path}");

                        // Reuse the broken slot; it can never be decoded to a key.
                        self.classes
                            .get_mut(&size_class)
                            .unwrap()
                            .free
                            .push(slot_index);
                    }
                    SlotState::Occupied(slot) => {
                        if slot.version > max_version {
                            max_version = slot.version;
                        }

                        scanned.push(ScannedSlot {
                            key: (slot.table_name, slot.partition_key),
                            location: SlotLocation {
                                size_class,
                                slot_index,
                            },
                            version: slot.version,
                            payload: slot.payload,
                        });
                    }
                }
            }
        }

        // Deduplicate by key keeping the highest version; losers are freed.
        let mut winners: AHashMap<(String, String), ScannedSlot> = AHashMap::new();
        let mut losers: Vec<SlotLocation> = Vec::new();

        for slot in scanned {
            match winners.entry(slot.key.clone()) {
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    if slot.version > entry.get().version {
                        let old = entry.insert(slot);
                        losers.push(old.location);
                    } else {
                        losers.push(slot.location);
                    }
                }
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(slot);
                }
            }
        }

        for location in losers {
            self.zero_slot(location).await;
            self.classes
                .get_mut(&location.size_class)
                .unwrap()
                .free
                .push(location.slot_index);
        }

        // Seed the write counter above every version seen on disk so it stays
        // monotonic across restarts.
        self.next_version = max_version + 1;

        let mut result = Vec::with_capacity(winners.len());

        for (key, slot) in winners {
            self.index.insert(key.clone(), slot.location);
            result.push(LoadedPartition {
                table_name: key.0,
                partition_key: key.1,
                payload: slot.payload,
            });
        }

        result
    }

    // ---- writes ----------------------------------------------------------

    pub async fn save_partition(&mut self, table_name: &str, partition_key: &str, payload: &[u8]) {
        let needed = slot_bytes_needed(table_name, partition_key, payload.len());
        let size_class = size_class_for(needed);
        let key = (table_name.to_string(), partition_key.to_string());

        let version = self.next_version;
        self.next_version += 1;

        let existing = self.index.get(&key).copied();

        // Choose the target slot: in place when the size class is unchanged,
        // otherwise allocate or reuse a free one.
        let target = match existing {
            Some(location) if location.size_class == size_class => location,
            _ => self.allocate_slot(size_class),
        };

        self.index.insert(key, target);

        let buf = encode_slot(size_class, version, table_name, partition_key, payload);
        self.write_slot(target, &buf).await;

        if let Some(old) = existing
            && old != target
        {
            self.zero_slot(old).await;
            self.classes
                .get_mut(&old.size_class)
                .unwrap()
                .free
                .push(old.slot_index);
        }
    }

    pub async fn delete_partition(&mut self, table_name: &str, partition_key: &str) {
        let key = (table_name.to_string(), partition_key.to_string());

        if let Some(location) = self.index.remove(&key) {
            self.zero_slot(location).await;
            self.classes
                .get_mut(&location.size_class)
                .unwrap()
                .free
                .push(location.slot_index);
        }
    }

    pub async fn save_table_metadata(
        &mut self,
        table_name: &str,
        contract: TableMetadataFileContract,
    ) {
        self.tables.insert(table_name.to_string(), contract);
        self.persist_tables_meta().await;
    }

    pub async fn delete_table_metadata(&mut self, table_name: &str) {
        if self.tables.remove(table_name).is_some() {
            self.persist_tables_meta().await;
        }
    }

    /// Removes the whole folder. The in-memory bookkeeping goes with it, so a
    /// repo which somehow gets used again finds nothing rather than pointing at
    /// files that are not there.
    ///
    /// A folder which stays is **returned**, not printed: the caller has already
    /// taken the namespace out of the server, nothing retries what is left, and
    /// a leftover folder is a whole copy of the data the next start loads back -
    /// so answering "deleted" to that would be answering wrong.
    pub async fn delete_everything(&mut self) -> Result<(), String> {
        self.index.clear();
        self.classes.clear();
        self.tables.clear();

        match tokio::fs::remove_dir_all(&self.root).await {
            Ok(_) => Ok(()),
            // Nothing to remove is the outcome that was asked for.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(format!(
                "the folder {} can not be deleted: {err}",
                self.root
            )),
        }
    }

    /// Reclaims disk in two phases. (1) Any page-file whose every slot is free is
    /// fully dead, so the `<size>` file is removed and the class dropped from
    /// memory; a future write to that size class recreates the file from
    /// scratch. (2) A file with more than two slots where at least half are free
    /// is compacted: live slots from the tail move into the free slots at the
    /// head and the file is truncated to exactly its live slots. Other files are
    /// left untouched - their free slots are reused in place. Runs under the
    /// repo mutex, so it never races a write.
    pub async fn vacuum(&mut self) {
        let empty_classes: Vec<u32> = self
            .classes
            .iter()
            .filter(|(_, state)| {
                if state.slot_count == 0 {
                    return false;
                }

                let free: std::collections::HashSet<u64> = state.free.iter().copied().collect();
                (0..state.slot_count).all(|slot_index| free.contains(&slot_index))
            })
            .map(|(size_class, _)| *size_class)
            .collect();

        for size_class in empty_classes {
            // Treat "already gone" as success.
            let removed = match tokio::fs::remove_file(self.class_path(size_class)).await {
                Ok(_) => true,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
                Err(err) => {
                    println!(
                        "files_repo: could not vacuum page-file {size_class}: {err} (will retry)"
                    );
                    false
                }
            };

            if removed {
                self.classes.remove(&size_class);
                println!("files_repo: vacuumed fully-freed page-file for size class {size_class}");
            }
        }

        let compact_candidates: Vec<u32> = self
            .classes
            .iter()
            .filter(|(_, state)| {
                // Fully-free classes are phase 1's job: if their file removal
                // failed above, compacting them here would just re-hit the same
                // broken file.
                state.slot_count > 2
                    && (state.free.len() as u64) < state.slot_count
                    && state.free.len() as u64 * 2 >= state.slot_count
            })
            .map(|(size_class, _)| *size_class)
            .collect();

        for size_class in compact_candidates {
            self.compact_class(size_class).await;
        }
    }

    /// Compacts a half-empty page-file: every live slot sitting beyond the new
    /// end of file is copied verbatim (crc and version included, so the copy is
    /// valid by construction) into a free slot at the head, the in-memory index
    /// is repointed, and the file is truncated to exactly its live slots.
    ///
    /// Crash safety, in copy-fsync-truncate order: a torn copy fails its crc on
    /// the next scan and is skipped while the original tail slot still holds the
    /// data; a completed copy that crashes before the truncate leaves two valid
    /// slots with the SAME version - the head copy is scanned first and wins the
    /// dedup, the tail original is zeroed as the loser. The copies are fsynced
    /// BEFORE the truncate so the filesystem can never make the truncate durable
    /// ahead of the copied data.
    async fn compact_class(&mut self, size_class: u32) {
        let state = self.classes.get(&size_class).unwrap();
        let slot_count = state.slot_count;
        let live_count = slot_count - state.free.len() as u64;

        // Copy targets: free slots below the new end of file.
        let head_frees: Vec<u64> = state
            .free
            .iter()
            .copied()
            .filter(|slot_index| *slot_index < live_count)
            .collect();

        // Slots to move: live slots at or beyond the new end of file.
        let tail_lives: Vec<((String, String), u64)> = self
            .index
            .iter()
            .filter(|(_, location)| {
                location.size_class == size_class && location.slot_index >= live_count
            })
            .map(|(key, location)| (key.clone(), location.slot_index))
            .collect();

        // Live and free slots partition 0..slot_count, so the tail lives and the
        // head frees always pair up one to one.
        assert_eq!(
            tail_lives.len(),
            head_frees.len(),
            "files_repo: free-list / index mismatch in size class {size_class}"
        );

        let path = self.class_path(size_class);
        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .await
            .expect("files_repo: can not open page-file for compaction");

        let mut buf = vec![0u8; size_class as usize];

        for ((_, from_index), to_index) in tail_lives.iter().zip(head_frees.iter()) {
            file.seek(std::io::SeekFrom::Start(from_index * size_class as u64))
                .await
                .expect("files_repo: seek failed");
            file.read_exact(&mut buf)
                .await
                .expect("files_repo: compaction read failed");
            file.seek(std::io::SeekFrom::Start(to_index * size_class as u64))
                .await
                .expect("files_repo: seek failed");
            file.write_all(&buf)
                .await
                .expect("files_repo: compaction write failed");
        }

        file.flush()
            .await
            .expect("files_repo: compaction flush failed");
        file.sync_all()
            .await
            .expect("files_repo: compaction fsync failed");
        file.set_len(live_count * size_class as u64)
            .await
            .expect("files_repo: compaction truncate failed");

        for ((key, _), to_index) in tail_lives.into_iter().zip(head_frees) {
            self.index.insert(
                key,
                SlotLocation {
                    size_class,
                    slot_index: to_index,
                },
            );
        }

        let state = self.classes.get_mut(&size_class).unwrap();
        state.slot_count = live_count;
        state.free.clear();

        println!(
            "files_repo: compacted page-file {size_class}: {slot_count} -> {live_count} slots"
        );
    }

    // ---- low-level helpers ----------------------------------------------

    /// Reserves a slot index in `size_class`, reusing a freed one when
    /// available, otherwise extending the file by one slot.
    fn allocate_slot(&mut self, size_class: u32) -> SlotLocation {
        let state = self.classes.entry(size_class).or_insert(ClassState {
            slot_count: 0,
            free: Vec::new(),
        });

        if let Some(slot_index) = state.free.pop() {
            return SlotLocation {
                size_class,
                slot_index,
            };
        }

        let slot_index = state.slot_count;
        state.slot_count += 1;

        SlotLocation {
            size_class,
            slot_index,
        }
    }

    async fn write_slot(&self, location: SlotLocation, buf: &[u8]) {
        let path = self.class_path(location.size_class);

        let mut file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            // Never truncate: we overwrite one slot in place and keep the rest of
            // the page-file intact.
            .truncate(false)
            .open(&path)
            .await
            .expect("files_repo: can not open page-file for write");

        file.seek(std::io::SeekFrom::Start(location.offset()))
            .await
            .expect("files_repo: seek failed");
        file.write_all(buf)
            .await
            .expect("files_repo: slot write failed");
        // tokio buffers the write and hands it to the blocking pool; flush waits
        // for that write to land in the OS (it does NOT fsync), so reads through
        // other fds in this process observe it. Power-loss durability stays
        // best-effort by design.
        file.flush().await.expect("files_repo: slot flush failed");
    }

    /// Marks a slot free by zeroing its 16-byte prefix (`body_len = 0`), so the
    /// recovery scan treats it as reusable and never reloads its old content.
    async fn zero_slot(&self, location: SlotLocation) {
        let path = self.class_path(location.size_class);

        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .expect("files_repo: can not open page-file to free a slot");

        file.seek(std::io::SeekFrom::Start(location.offset()))
            .await
            .expect("files_repo: seek failed");
        file.write_all(&[0u8; SLOT_PREFIX_LEN])
            .await
            .expect("files_repo: zeroing slot failed");
        file.flush().await.expect("files_repo: slot flush failed");
    }

    async fn persist_tables_meta(&self) {
        let yaml = serde_yaml::to_string(&self.tables).unwrap();
        atomic_write(
            &format!("{}/{}", self.root, TABLES_META_FILE),
            yaml.as_bytes(),
        )
        .await;
    }
}

/// Reads `tables.meta`. A missing file is a fresh directory (empty map). A read
/// error or a file that does not parse is fatal unless `skip_errors`
/// (SkipBrokenPartitions) is set - silently defaulting would reset every table's
/// attributes and let the next metadata write overwrite the still-recoverable
/// file.
async fn load_tables_meta(
    root: &str,
    skip_errors: bool,
) -> BTreeMap<String, TableMetadataFileContract> {
    let path = format!("{root}/{TABLES_META_FILE}");

    let bytes = match tokio::fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
        Err(err) => panic!("files_repo: can not read {path}: {err}"),
    };

    match serde_yaml::from_slice(&bytes) {
        Ok(tables) => tables,
        Err(err) => {
            let msg = format!("files_repo: can not parse {path} as yaml ({err})");

            if skip_errors {
                println!("{msg}. Table attributes will be restored with defaults.");
                BTreeMap::new()
            } else {
                panic!("{msg}");
            }
        }
    }
}

/// Lists the persistence root and records every page-file's slot count. Any
/// listing / stat error is fatal - silently dropping a size class here would
/// bypass the recovery scan exactly like an unreadable page-file. Files whose
/// name is not a plain integer (`tables.meta`) are not page-files and are
/// skipped.
async fn discover_class_files(root: &str) -> AHashMap<u32, ClassState> {
    let mut classes = AHashMap::new();

    let mut read_dir = tokio::fs::read_dir(root)
        .await
        .unwrap_or_else(|err| panic!("files_repo: can not list persistence root {root}: {err}"));

    loop {
        let entry = read_dir.next_entry().await.unwrap_or_else(|err| {
            panic!("files_repo: can not list persistence root {root}: {err}")
        });

        let Some(entry) = entry else {
            break;
        };

        let file_type = entry
            .file_type()
            .await
            .unwrap_or_else(|err| panic!("files_repo: can not stat {:?}: {}", entry.path(), err));

        if !file_type.is_file() {
            continue;
        }

        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };

        let Ok(size_class) = file_name.parse::<u32>() else {
            continue;
        };

        if size_class < MIN_SIZE_CLASS {
            continue;
        }

        let len = entry
            .metadata()
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "files_repo: can not stat page-file {:?}: {}",
                    entry.path(),
                    err
                )
            })
            .len();

        classes.insert(
            size_class,
            ClassState {
                slot_count: len / size_class as u64,
                free: Vec::new(),
            },
        );
    }

    classes
}

/// Writes `bytes` to `path` atomically: tmp file -> fsync -> rename. Used for
/// `tables.meta`, where a torn write would be costly; the slots themselves are
/// deliberately written without fsync.
pub async fn atomic_write(path: &str, bytes: &[u8]) {
    let tmp_path = format!("{path}.tmp");

    {
        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .expect("files_repo: can not create tmp file");
        file.write_all(bytes)
            .await
            .expect("files_repo: tmp write failed");
        file.sync_all().await.expect("files_repo: tmp fsync failed");
    }

    tokio::fs::rename(&tmp_path, path)
        .await
        .expect("files_repo: rename failed");
}
