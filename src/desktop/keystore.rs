use std::fs::{File, Metadata, OpenOptions};
use std::path::{Component, Path, PathBuf};

pub fn default_root() -> Result<PathBuf, String> {
    if let Some(root) = std::env::var_os("CHASM_KEYSTORE_DIR") {
        let root = PathBuf::from(root);
        if !root.is_absolute() { return Err("CHASM_KEYSTORE_DIR must be an absolute folder path.".into()); }
        return Ok(root);
    }
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from).filter(|path| path.is_absolute())
        .ok_or("Could not resolve the home directory for keystores.")?;
    Ok(home.join(".foundry").join("keystores"))
}

pub fn validate_filename(name: &str) -> Result<(), String> {
    let path = Path::new(name);
    let mut parts = path.components();
    if name.is_empty() || name.ends_with([' ', '.'])
        || name.chars().any(|c| c.is_control() || "\\/:*?\"<>|".contains(c))
        || !matches!(parts.next(), Some(Component::Normal(_))) || parts.next().is_some()
    {
        return Err("Choose a file directly inside the keystore folder.".into());
    }
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| stem.strip_prefix(prefix)
            .map(|suffix| matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")).unwrap_or(false))
    {
        return Err("Windows device names cannot be used as keystore files.".into());
    }
    Ok(())
}

fn checked_metadata(path: &Path, directory: bool) -> Result<Metadata, String> {
    let metadata = path.symlink_metadata().map_err(|error| format!("Could not inspect {}: {error}", path.display()))?;
    #[cfg(windows)]
    let linked = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0
    };
    #[cfg(not(windows))]
    let linked = metadata.file_type().is_symlink();
    if linked || (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err("Keystore actions require regular files and folders, not links or reparse points.".into());
    }
    Ok(metadata)
}

#[derive(Clone)]
pub struct KeystoreStore {
    root: PathBuf,
}

impl KeystoreStore {
    pub fn new(root: PathBuf) -> Self { Self { root } }

    pub fn directory(&self) -> Result<PathBuf, String> {
        if !self.root.is_absolute() || self.root.components().any(|part| matches!(part, Component::ParentDir)) {
            return Err("The keystore folder must be an absolute local filesystem path.".into());
        }
        for ancestor in self.root.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if !ancestor.as_os_str().is_empty() { checked_metadata(ancestor, true)?; }
        }
        Ok(self.root.clone())
    }

    pub fn ensure_directory(&self) -> Result<PathBuf, String> {
        if !self.root.is_absolute() || self.root.components().any(|part| matches!(part, Component::ParentDir)) {
            return Err("The keystore folder must be an absolute local filesystem path.".into());
        }
        for ancestor in self.root.ancestors().collect::<Vec<_>>().into_iter().rev() {
            if ancestor.as_os_str().is_empty() { continue; }
            match ancestor.symlink_metadata() {
                Ok(_) => { checked_metadata(ancestor, true)?; }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir(ancestor).map_err(|error| format!("Could not create the keystore folder: {error}"))?;
                    checked_metadata(ancestor, true)?;
                }
                Err(error) => return Err(format!("Could not inspect the keystore folder: {error}")),
            }
        }
        Ok(self.root.clone())
    }

    pub fn list(&self) -> Result<Vec<String>, String> {
        let root = self.directory()?;
        let entries = std::fs::read_dir(root).map_err(|error| format!("Could not read the keystore folder: {error}"))?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| format!("Could not read a keystore entry: {error}"))?;
            let Some(name) = entry.file_name().to_str().map(str::to_string) else { continue };
            if validate_filename(&name).is_ok() && checked_metadata(&entry.path(), false).is_ok() {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn file_path(&self, name: &str) -> Result<PathBuf, String> {
        validate_filename(name)?;
        let path = self.directory()?.join(name);
        checked_metadata(&path, false)?;
        Ok(path)
    }

    pub fn prepare_delete(&self, name: &str) -> Result<PendingDelete, String> {
        let path = self.file_path(name)?;
        let protected = protected::Target::open(&self.root, &path)?;
        Ok(PendingDelete { name: name.into(), path, protected })
    }
}

pub struct PendingDelete {
    name: String,
    path: PathBuf,
    protected: protected::Target,
}

impl PendingDelete {
    pub fn name(&self) -> &str { &self.name }
    pub fn path(&self) -> &Path { &self.path }
    pub fn delete(self) -> Result<(), String> { self.protected.delete() }
}

#[cfg(windows)]
mod protected {
    use super::*;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::*;

    pub struct Target {
        file: File,
        _parents: Vec<File>,
        identity: (u32, u32, u32),
    }

    fn inspect(file: &File, directory: bool) -> Result<BY_HANDLE_FILE_INFORMATION, String> {
        let mut info = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(format!("Could not inspect the keystore file handle: {}", std::io::Error::last_os_error()));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
            || (!directory && info.nNumberOfLinks != 1)
        {
            return Err("Keystore deletion does not follow links, reparse points, or hard links.".into());
        }
        Ok(info)
    }

    impl Target {
        pub fn open(root: &Path, path: &Path) -> Result<Self, String> {
            let mut parents = Vec::new();
            for parent in root.ancestors().collect::<Vec<_>>().into_iter().rev() {
                if parent.as_os_str().is_empty() { continue; }
                let file = OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(parent).map_err(|error| format!("Could not protect the keystore folder: {error}"))?;
                inspect(&file, true)?;
                parents.push(file);
            }
            let file = OpenOptions::new().access_mode(DELETE | FILE_READ_ATTRIBUTES)
                .share_mode(FILE_SHARE_READ).custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
                .open(path).map_err(|error| format!("Could not prepare deletion. Close programs using this keystore and try again: {error}"))?;
            let info = inspect(&file, false)?;
            let metadata = file.metadata().map_err(|error| format!("Could not inspect the keystore: {error}"))?;
            if metadata.file_attributes() & FILE_ATTRIBUTE_READONLY != 0 {
                return Err("The keystore is read-only. Change its permissions before deleting it.".into());
            }
            Ok(Self { file, _parents: parents, identity: (info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow) })
        }

        pub fn delete(self) -> Result<(), String> {
            let info = inspect(&self.file, false)?;
            if self.identity != (info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow) {
                return Err("The keystore changed. Select it again before deleting.".into());
            }
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
            if unsafe { SetFileInformationByHandle(self.file.as_raw_handle(), FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(), std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32) } == 0
            {
                return Err(format!("Could not delete the keystore: {}", std::io::Error::last_os_error()));
            }
            Ok(())
        }
    }
}

#[cfg(not(windows))]
mod protected {
    use super::*;
    pub struct Target;
    impl Target {
        pub fn open(_: &Path, _: &Path) -> Result<Self, String> {
            Err("Identity-bound keystore deletion is currently available on Windows only.".into())
        }
        pub fn delete(self) -> Result<(), String> { Err("Keystore deletion is not supported on this platform.".into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_names_allow_spaces_and_json_but_not_paths() {
        for name in ["local-dev", "wallet.json", "Wallet 1", "é.json"] { assert!(validate_filename(name).is_ok(), "{name}"); }
        for name in ["", "..", "../key", "C:\\key", "a/b", "a\\b", "a:stream", "CON", "NUL.json", "COM1", "a.", "a "] {
            assert!(validate_filename(name).is_err(), "{name}");
        }
    }

    #[test]
    fn listing_only_returns_direct_regular_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("Wallet 1.json"), "encrypted fixture").unwrap();
        std::fs::create_dir(root.path().join("subdir")).unwrap();
        let store = KeystoreStore::new(root.path().to_path_buf());
        assert_eq!(store.list().unwrap(), vec!["Wallet 1.json"]);
        assert!(store.file_path("../other").is_err());
        assert!(store.prepare_delete("subdir").is_err());
    }

    #[cfg(windows)]
    #[test]
    fn confirmation_owns_exact_file_and_cancel_does_nothing() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("one.json");
        let sibling = root.path().join("two.json");
        std::fs::write(&target, "encrypted one").unwrap();
        std::fs::write(&sibling, "encrypted two").unwrap();
        let store = KeystoreStore::new(root.path().to_path_buf());
        let pending = store.prepare_delete("one.json").unwrap();
        assert!(std::fs::rename(&target, root.path().join("renamed")).is_err());
        assert!(std::fs::write(&target, "replacement").is_err());
        drop(pending);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "encrypted one");
        store.prepare_delete("one.json").unwrap().delete().unwrap();
        assert!(!target.exists());
        assert_eq!(std::fs::read_to_string(sibling).unwrap(), "encrypted two");
    }

    #[cfg(windows)]
    #[test]
    fn locked_file_fails_without_changing_bytes() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("locked");
        std::fs::write(&path, "encrypted fixture").unwrap();
        let _locked = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).unwrap();
        assert!(KeystoreStore::new(root.path().to_path_buf()).prepare_delete("locked").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "encrypted fixture");
    }
}
