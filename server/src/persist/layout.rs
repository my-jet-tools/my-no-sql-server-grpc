/// Every namespace persists into a folder of its own inside the persistence
/// root, the default one included:
///
/// ```text
/// <root>/default/512, /1024, /tables.meta
/// <root>/alpha/512,   /1024, /tables.meta
/// ```
pub fn get_namespace_folder(root: &str, namespace: &str) -> String {
    format!("{root}/{namespace}")
}

/// A namespace name becomes a folder, so this is what stands between a caller
/// and the file system: a name which is not one this server would have written
/// itself is refused rather than resolved. It is also what keeps a name from
/// being written and then never found again - a folder this says no to is
/// skipped at the next start, and every row in it would be unreachable.
pub fn is_valid_namespace_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }

    // A name made of nothing but dots is `.` or `..` - the folder itself and
    // the one above it. Both pass every other check here, and `..` is one level
    // out of whatever folder the name is resolved inside.
    if name.chars().all(|c| c == '.') {
        return false;
    }

    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Namespaces which already have a folder on disk. Called at start up to load
/// every namespace this server persisted before.
pub async fn get_namespaces_on_disk(root: &str) -> Vec<String> {
    let mut result = Vec::new();

    let Ok(mut read_dir) = tokio::fs::read_dir(root).await else {
        // No root yet - a first ever start. The folder is created when the
        // namespace's persistence is opened.
        return result;
    };

    loop {
        let entry = read_dir.next_entry().await.unwrap_or_else(|err| {
            panic!("Can not list the persistence root {root} while looking for namespaces: {err}")
        });

        let Some(entry) = entry else {
            break;
        };

        let is_dir = match entry.file_type().await {
            Ok(file_type) => file_type.is_dir(),
            Err(_) => false,
        };

        if !is_dir {
            continue;
        }

        let folder_name = entry.file_name().to_string_lossy().to_string();

        if !is_valid_namespace_name(&folder_name) {
            println!(
                "WARNING: folder {folder_name} inside the persistence root {root} is not a valid namespace name and is skipped"
            );
            continue;
        }

        result.push(folder_name);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_which_could_step_out_of_the_root_is_not_a_namespace_name() {
        assert!(is_valid_namespace_name("default"));
        assert!(is_valid_namespace_name("archive-2026"));
        assert!(is_valid_namespace_name("my.name_space"));

        assert!(!is_valid_namespace_name(".."));
        assert!(!is_valid_namespace_name("."));
        assert!(!is_valid_namespace_name("..."));
        assert!(!is_valid_namespace_name("../../../../tmp/pwn"));
        assert!(!is_valid_namespace_name("with/slash"));
        assert!(!is_valid_namespace_name("with\\backslash"));
        assert!(!is_valid_namespace_name(""));
    }

    /// A name which is not refused becomes a folder, and a folder the start up
    /// scan skips is a folder whose rows nobody can reach any more - so the
    /// benign shapes have to be refused for the very same reason as `..`.
    #[test]
    fn a_name_which_would_be_skipped_at_the_next_start_is_refused_now() {
        assert!(!is_valid_namespace_name("with space"));
        assert!(!is_valid_namespace_name(&"a".repeat(65)));

        assert!(is_valid_namespace_name(&"a".repeat(64)));
    }
}
