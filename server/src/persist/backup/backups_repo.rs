use rust_extensions::date_time::DateTimeAsMicroseconds;
use tokio::io::AsyncWriteExt;

/// How many backups of one namespace can be taken inside one second. The stamp a
/// name is made of has a second of resolution, so this is how far the suffix
/// which tells them apart counts.
const MAX_BACKUPS_PER_SECOND: usize = 99;

/// A backup on disk.
pub struct BackupOnDisk {
    /// What every other call names it by.
    pub name: String,
    pub size: u64,
}

/// Where backups are kept.
///
/// Laid out the way the persistence root is - a folder per namespace, the
/// default one included - so a namespace's snapshots are its own and how many to
/// keep is counted per namespace.
///
/// **Not** inside the persistence root: every folder there is loaded as a
/// namespace at start up, so a `backups` folder next to them would come back as
/// a namespace called "backups". It is a setting of its own, which is also what
/// an operator wants - a backup on the same disk as the data is half a backup.
///
/// Without that setting there is no backups folder at all and every call says
/// so. Writing gigabytes into a path nobody named is worse than refusing.
pub struct BackupsRepo {
    folder: Option<String>,
}

impl BackupsRepo {
    pub fn new(folder: Option<String>) -> Self {
        Self { folder }
    }

    pub fn is_configured(&self) -> bool {
        self.folder.is_some()
    }

    pub async fn save(
        &self,
        name_space: &str,
        content: &[u8],
        now: DateTimeAsMicroseconds,
    ) -> Result<String, String> {
        let folder = self.get_folder(name_space)?;

        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|err| format!("can not create the backups folder {folder}: {err}"))?;

        let reserved = reserve_name(&folder, now).await?;

        // The same tmp-fsync-rename the schemas file uses: a backup which was
        // interrupted must not be listed as one that can be restored. Unlike the
        // page-files it is **not** the one in `files_repo_inner`, which panics:
        // a page-file which can not be written is a server which lost its data
        // either way, while a backups disk which filled up is what
        // `BackupFailed` is for - and taking the backup timer's task down with
        // it is how the next backup never happens either.
        write_and_rename(&folder, reserved, content).await
    }

    pub async fn get_all(&self, name_space: &str) -> Result<Vec<BackupOnDisk>, String> {
        let folder = self.get_folder(name_space)?;

        let mut read_dir = match tokio::fs::read_dir(&folder).await {
            Ok(read_dir) => read_dir,
            // Nothing has been backed up yet, which is not a failure.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(format!("can not list the backups folder {folder}: {err}")),
        };

        let mut result = Vec::new();

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();

            if !is_valid_name(&name) {
                continue;
            }

            let size = match entry.metadata().await {
                Ok(metadata) => metadata.len(),
                Err(_) => continue,
            };

            result.push(BackupOnDisk { name, size });
        }

        // The name carries the moment it was taken, so sorting by it is sorting
        // by when they happened.
        result.sort_by(|left, right| left.name.cmp(&right.name));

        Ok(result)
    }

    pub async fn read(&self, name_space: &str, name: &str) -> Result<Vec<u8>, String> {
        let folder = self.get_folder(name_space)?;

        if !is_valid_name(name) {
            return Err(format!("'{name}' is not the name of a backup"));
        }

        tokio::fs::read(format!("{folder}/{name}"))
            .await
            .map_err(|err| format!("can not read the backup {name}: {err}"))
    }

    /// Keeps the newest `max_backups` of one namespace and removes the rest.
    /// Returns what went.
    pub async fn keep_last(
        &self,
        name_space: &str,
        max_backups: usize,
    ) -> Result<Vec<String>, String> {
        let folder = self.get_folder(name_space)?;
        let all = self.get_all(name_space).await?;

        if all.len() <= max_backups {
            return Ok(Vec::new());
        }

        let mut removed = Vec::new();

        for backup in all.iter().take(all.len() - max_backups) {
            if tokio::fs::remove_file(format!("{folder}/{}", backup.name))
                .await
                .is_ok()
            {
                removed.push(backup.name.clone());
            }
        }

        Ok(removed)
    }

    fn get_folder(&self, name_space: &str) -> Result<String, String> {
        let Some(folder) = self.folder.as_ref() else {
            return Err(
                "Backups are not configured: set BackupsDest in the settings to the folder they belong in"
                    .to_string(),
            );
        };

        let name_space = crate::app::DbNamespaces::resolve_name(name_space);

        // The namespace comes from a caller and becomes a path. A name which is
        // not one this server would have written is refused rather than
        // resolved - it could otherwise step out of the backups folder
        // altogether.
        if !crate::persist::layout::is_valid_namespace_name(name_space) {
            return Err(format!("'{name_space}' is not a namespace name"));
        }

        Ok(format!(
            "{}/{name_space}",
            folder.trim_end_matches(['/', '\\'])
        ))
    }
}

/// The name carries the moment it was taken, so the names sort the way the
/// backups happened: `20260810T071415.zip`.
///
/// The stamp counts in seconds, and a manual backup lands in the same second as
/// the timer's often enough - the timer takes one on the interval an operator
/// asked for, and an operator who wants one now asks for it around then. The
/// second and later of one second are `_02`, `_03`: without them the newer one
/// silently overwrote the older, and `MaxBackups` counted the two as one. The
/// suffix sorts after the bare name and before the next second's, so sorting by
/// name is still sorting by when they happened.
fn compile_name(now: DateTimeAsMicroseconds, attempt: usize) -> String {
    let stamp = now.to_rfc3339().replace([':', '-'], "");
    let stamp = &stamp[..15];

    if attempt < 2 {
        return format!("{stamp}.zip");
    }

    format!("{stamp}_{attempt:02}.zip")
}

/// Picks the name a backup lands under and **takes** it: the tmp file it will be
/// written through is created exclusively, so two backups of the same second
/// racing each other end up with a name each rather than with the last writer's
/// bytes under one name. Nothing else can be listed as a backup meanwhile - a
/// tmp file is not a `.zip`.
async fn reserve_name(folder: &str, now: DateTimeAsMicroseconds) -> Result<ReservedName, String> {
    for attempt in 1..=MAX_BACKUPS_PER_SECOND {
        let name = compile_name(now, attempt);
        let tmp_path = format!("{folder}/{name}.tmp");

        if tokio::fs::try_exists(format!("{folder}/{name}"))
            .await
            .unwrap_or(false)
        {
            continue;
        }

        match tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .await
        {
            Ok(file) => {
                return Ok(ReservedName {
                    name,
                    tmp_path,
                    file,
                });
            }
            // A backup of this second is being written right this moment, which
            // is the same collision as one which finished already.
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(format!("can not write the backup {name}: {err}")),
        }
    }

    Err(format!(
        "{MAX_BACKUPS_PER_SECOND} backups of this namespace were already taken this second"
    ))
}

struct ReservedName {
    name: String,
    tmp_path: String,
    file: tokio::fs::File,
}

async fn write_and_rename(
    folder: &str,
    reserved: ReservedName,
    content: &[u8],
) -> Result<String, String> {
    let ReservedName {
        name,
        tmp_path,
        mut file,
    } = reserved;

    let mut result = write_and_sync(&mut file, content).await;
    drop(file);

    if result.is_ok() {
        result = tokio::fs::rename(&tmp_path, format!("{folder}/{name}"))
            .await
            .map_err(|err| format!("can not put the backup {name} into place: {err}"));
    }

    if let Err(err) = result {
        // The name was taken by creating this file, so a write which did not
        // happen gives it back - otherwise every later backup of this second
        // steps over a reservation nobody wrote anything into.
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(err);
    }

    Ok(name)
}

async fn write_and_sync(file: &mut tokio::fs::File, content: &[u8]) -> Result<(), String> {
    file.write_all(content)
        .await
        .map_err(|err| format!("can not write the backup: {err}"))?;

    file.sync_all()
        .await
        .map_err(|err| format!("can not flush the backup to the disk: {err}"))
}

/// A snapshot is addressed by its bare file name inside its namespace's own
/// folder. Anything with a path in it would step out of that folder - into
/// another namespace's backups, or out of the backups folder altogether - so it
/// is refused rather than resolved.
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains("..")
        && name.ends_with(".zip")
}

#[cfg(test)]
mod tests {
    use super::*;

    static FOLDER_NO: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn new_test_folder() -> String {
        let folder = std::env::temp_dir().join(format!(
            "my-no-sql-grpc-backups-{}-{}",
            std::process::id(),
            FOLDER_NO.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));

        let _ = std::fs::remove_dir_all(&folder);
        folder.to_string_lossy().to_string()
    }

    #[test]
    fn a_name_is_the_moment_it_was_taken() {
        let name = compile_name(DateTimeAsMicroseconds::new(1_786_346_055_530_973), 1);

        assert!(name.ends_with(".zip"));
        assert_eq!(name.len(), "20260810T071415.zip".len());
        assert!(is_valid_name(&name));
    }

    /// Sorting the names is sorting the backups by when they were taken, and
    /// `keep_last` throws away the head of that order - so a second backup of one
    /// second must sort after the first one and before the next second's.
    #[test]
    fn a_second_backup_of_one_second_sorts_after_the_first() {
        let now = DateTimeAsMicroseconds::new(1_786_346_055_530_973);
        let next_second = DateTimeAsMicroseconds::new(1_786_346_056_530_973);

        let first = compile_name(now, 1);
        let second = compile_name(now, 2);
        let tenth = compile_name(now, 10);

        assert!(is_valid_name(&second) && is_valid_name(&tenth));

        assert!(first < second);
        assert!(second < tenth);
        assert!(tenth < compile_name(next_second, 1));
    }

    /// A manual backup lands in the same second as the timer's often enough, and
    /// the one which lost used to be the one which happened first.
    #[tokio::test]
    async fn two_backups_of_one_second_are_two_backups() {
        let folder = new_test_folder();
        let repo = BackupsRepo::new(Some(folder.clone()));
        let now = DateTimeAsMicroseconds::new(1_786_346_055_530_973);

        let first = repo.save("alpha", b"the timer's", now).await.unwrap();
        let second = repo.save("alpha", b"the operator's", now).await.unwrap();

        assert_ne!(first, second);

        let all = repo.get_all("alpha").await.unwrap();
        assert_eq!(all.len(), 2);

        assert_eq!(repo.read("alpha", &first).await.unwrap(), b"the timer's");
        assert_eq!(
            repo.read("alpha", &second).await.unwrap(),
            b"the operator's"
        );

        let _ = std::fs::remove_dir_all(&folder);
    }

    /// A backups disk which can not be written is what `BackupFailed` is for.
    /// Panicking here takes the backup timer's task down, and then the backup
    /// after this one never happens either.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_disk_which_refuses_the_write_is_reported_not_panicked() {
        use std::os::unix::fs::PermissionsExt;

        let folder = new_test_folder();
        std::fs::create_dir_all(format!("{folder}/alpha")).unwrap();
        std::fs::set_permissions(
            format!("{folder}/alpha"),
            std::fs::Permissions::from_mode(0o555),
        )
        .unwrap();

        let repo = BackupsRepo::new(Some(folder.clone()));

        assert!(
            repo.save("alpha", b"x", DateTimeAsMicroseconds::now())
                .await
                .is_err()
        );

        std::fs::set_permissions(
            format!("{folder}/alpha"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn nothing_with_a_path_in_it_is_a_backup_name() {
        assert!(is_valid_name("20260810T071415.zip"));

        assert!(!is_valid_name("../../etc/passwd.zip"));
        assert!(!is_valid_name("other-namespace/20260810T071415.zip"));
        assert!(!is_valid_name("..\\backup.zip"));
        assert!(!is_valid_name("20260810T071415"));
        assert!(!is_valid_name(""));
    }

    #[test]
    fn a_namespace_which_is_not_a_name_can_not_become_a_path() {
        let repo = BackupsRepo::new(Some("/tmp/backups".to_string()));

        assert_eq!(repo.get_folder("").unwrap(), "/tmp/backups/default");
        assert_eq!(repo.get_folder("alpha").unwrap(), "/tmp/backups/alpha");

        assert!(repo.get_folder("../../etc").is_err());
        assert!(repo.get_folder("with/slash").is_err());

        // A name of nothing but dots carries no separator, so it passes every
        // other check - and `..` alone is still one level out of the backups
        // folder, which is where every namespace's snapshots sit side by side.
        assert!(repo.get_folder("..").is_err());
        assert!(repo.get_folder(".").is_err());
        assert!(repo.get_folder("...").is_err());
    }

    #[tokio::test]
    async fn every_call_says_so_when_there_is_nowhere_to_put_them() {
        let repo = BackupsRepo::new(None);

        assert!(!repo.is_configured());
        assert!(
            repo.save("default", b"x", DateTimeAsMicroseconds::now())
                .await
                .is_err()
        );
        assert!(repo.get_all("default").await.is_err());
        assert!(repo.read("default", "20260810T071415.zip").await.is_err());
    }
}
