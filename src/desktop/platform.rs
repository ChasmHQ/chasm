use std::path::{Path, PathBuf};

pub use native::{error_message, open_folder, open_json_file, pick_folder_with_owner, reveal_file, save_json_file, save_text_file};

pub fn show_error(title: &str, message: &str) {
    if let Err(error) = error_message(title, message) {
        eprintln!("{title}: {message}\n{error}");
    }
}

pub fn validate_workspace(path: &Path) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("Choose an existing workspace folder.".into());
    }
    let root = path.canonicalize().map_err(|error| format!("Could not resolve the workspace folder: {error}"))?;
    let metadata = root.metadata().map_err(|error| format!("Could not inspect the workspace folder: {error}"))?;
    if !metadata.is_dir() {
        return Err("The workspace path must be a folder.".into());
    }
    Ok(root)
}

/// A physical-pixel monitor work area, with an exclusive right and bottom edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

/// Keep the logical client size and physical outer-frame position within a work area.
/// The work area must accommodate the platform minimum client size.
pub fn clamp_panel_to_work_area(
    geometry: super::layout::Geometry,
    scale: f32,
    work: WorkArea,
) -> Option<super::layout::Geometry> {
    if !scale.is_finite() || scale <= 0.0 || work.right <= work.left || work.bottom <= work.top {
        return None;
    }
    let available_width = (i64::from(work.right) - i64::from(work.left)) as f64;
    let available_height = (i64::from(work.bottom) - i64::from(work.top)) as f64;
    let scale = f64::from(scale);
    let max_width = (available_width / scale).floor().min(8192.0) as u32;
    let max_height = (available_height / scale).floor().min(8192.0) as u32;
    if max_width < 240 || max_height < 160 {
        return None;
    }
    let width = geometry.width.clamp(240, max_width);
    let height = geometry.height.clamp(160, max_height);
    let physical_width = (f64::from(width) * scale).ceil() as i64;
    let physical_height = (f64::from(height) * scale).ceil() as i64;
    let x = i64::from(geometry.x).clamp(i64::from(work.left), i64::from(work.right) - physical_width);
    let y = i64::from(geometry.y).clamp(i64::from(work.top), i64::from(work.bottom) - physical_height);
    Some(super::layout::Geometry { x: x as i32, y: y as i32, width, height, ..geometry })
}

pub fn visible_panel_geometry(geometry: super::layout::Geometry, scale: f32) -> super::layout::Geometry {
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::RECT,
            Graphics::Gdi::{GetMonitorInfoW, MonitorFromRect, MONITORINFO, MONITOR_DEFAULTTONEAREST},
        };
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let width = (geometry.width as f32 * scale).ceil() as i32;
        let height = (geometry.height as f32 * scale).ceil() as i32;
        let rectangle = RECT {
            left: geometry.x,
            top: geometry.y,
            right: geometry.x.saturating_add(width),
            bottom: geometry.y.saturating_add(height),
        };
        let monitor = unsafe { MonitorFromRect(&rectangle, MONITOR_DEFAULTTONEAREST) };
        let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if unsafe { GetMonitorInfoW(monitor, &mut info) } != 0 {
            let work = info.rcWork;
            if let Some(visible) = clamp_panel_to_work_area(
                geometry,
                scale,
                WorkArea { left: work.left, top: work.top, right: work.right, bottom: work.bottom },
            ) {
                return visible;
            }
        }
    }
    let _ = scale;
    geometry
}

/// Blocking. Once the desktop event loop is running, call from a worker thread.
pub fn pick_folder(initial: Option<&Path>) -> Result<Option<PathBuf>, String> {
    pick_folder_with_owner(initial, None)
}

#[cfg(windows)]
mod native {
    use super::validate_workspace;
    use std::ffi::{c_void, OsStr, OsString};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Component, Path, PathBuf, Prefix};
    use windows::{
        core::{w, HRESULT, PCWSTR, PWSTR},
        Win32::{
            Foundation::{ERROR_CANCELLED, HWND},
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_APARTMENTTHREADED,
            },
            UI::{
                Shell::{
                    Common::{COMDLG_FILTERSPEC, ITEMIDLIST}, FileOpenDialog, FileSaveDialog,
                    IFileOpenDialog, IFileSaveDialog, IShellItem, SHCreateItemFromParsingName,
                    SHOpenFolderAndSelectItems, SHParseDisplayName, ShellExecuteW, FOS_FORCEFILESYSTEM,
                    FOS_NOCHANGEDIR, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST, FOS_PICKFOLDERS,
                    SIGDN_FILESYSPATH,
                },
                WindowsAndMessaging::{
                    IsWindow, MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TASKMODAL, SW_SHOWNORMAL,
                },
            },
        },
    };

    struct ComApartment(std::marker::PhantomData<std::rc::Rc<()>>);

    impl ComApartment {
        fn initialize() -> Result<Self, String> {
            unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
                .ok()
                .map_err(|error| format!("Could not initialize the Windows shell: {error}"))?;
            Ok(Self(std::marker::PhantomData))
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    struct TaskMem<T>(*mut T);

    impl<T> Drop for TaskMem<T> {
        fn drop(&mut self) {
            unsafe { CoTaskMemFree(Some(self.0.cast::<c_void>())) };
        }
    }

    fn run_sta<T: Send + 'static>(action: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
        std::thread::Builder::new()
            .name("chasm-shell".into())
            .spawn(move || {
                let _apartment = ComApartment::initialize()?;
                action()
            })
            .map_err(|error| format!("Could not start the Windows shell worker: {error}"))?
            .join()
            .map_err(|_| "The Windows shell worker stopped unexpectedly.".to_string())?
    }

    fn wide_null(text: &OsStr) -> Result<Vec<u16>, String> {
        let mut wide: Vec<u16> = text.encode_wide().collect();
        if wide.contains(&0) {
            return Err("Windows paths and dialog text cannot contain NUL characters.".into());
        }
        wide.push(0);
        Ok(wide)
    }

    fn shell_path(path: &Path) -> Result<Vec<u16>, String> {
        let mut wide = wide_null(path.as_os_str())?;
        if let Some(Component::Prefix(prefix)) = path.components().next() {
            // Explorer expects DOS/UNC paths rather than canonicalize's verbatim prefix.
            match prefix.kind() {
                Prefix::VerbatimDisk(_) => {
                    wide.drain(..4);
                }
                Prefix::VerbatimUNC(_, _) => {
                    wide.splice(..8, [b'\\' as u16; 2]);
                }
                Prefix::Verbatim(_) | Prefix::DeviceNS(_) => {
                    return Err("Windows shell actions require a filesystem path.".into());
                }
                _ => {}
            }
        }
        if path.components().any(|part| match part {
            Component::Normal(name) => matches!(name.encode_wide().last(), Some(32 | 46)),
            _ => false,
        }) {
            return Err("Windows Explorer cannot preserve trailing dots or spaces in a path component.".into());
        }
        Ok(wide)
    }

    /// Blocking. Keep the owner's UI thread pumping messages and the HWND alive until return.
    /// None (or zero) creates an unowned dialog, suitable before desktop startup.
    pub fn pick_folder_with_owner(initial: Option<&Path>, owner: Option<isize>) -> Result<Option<PathBuf>, String> {
        let initial = initial.map(Path::to_path_buf);
        run_sta(move || {
            let owner = owner.filter(|handle| *handle != 0).map(|handle| HWND(handle as *mut c_void));
            if owner.is_some() && !unsafe { IsWindow(owner) }.as_bool() {
                return Err("The folder picker owner window is no longer available.".into());
            }
            let dialog: IFileOpenDialog = unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| format!("Could not create the folder picker: {error}"))?;
            let options = unsafe { dialog.GetOptions() }
                .map_err(|error| format!("Could not read folder picker options: {error}"))?;
            unsafe {
                dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR)
            }
            .map_err(|error| format!("Could not configure the folder picker: {error}"))?;
            unsafe { dialog.SetTitle(w!("Open Folder")) }
                .map_err(|error| format!("Could not set the folder picker title: {error}"))?;
            if let Some(initial) = initial {
                let initial = shell_path(&validate_workspace(&initial)?)?;
                let item: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(initial.as_ptr()), None) }
                    .map_err(|error| format!("Could not resolve the initial folder: {error}"))?;
                unsafe { dialog.SetFolder(&item) }
                    .map_err(|error| format!("Could not set the initial folder: {error}"))?;
            }
            match unsafe { dialog.Show(owner) } {
                Ok(()) => {}
                Err(error) if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) => return Ok(None),
                Err(error) => return Err(format!("Could not show the folder picker: {error}")),
            }
            let item = unsafe { dialog.GetResult() }
                .map_err(|error| format!("Could not read the selected folder: {error}"))?;
            let name = TaskMem(
                unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
                    .map_err(|error| format!("The selected folder has no filesystem path: {error}"))?
                    .0,
            );
            if name.0.is_null() {
                return Err("The selected folder has no filesystem path.".into());
            }
            let path = PathBuf::from(OsString::from_wide(unsafe { PWSTR(name.0).as_wide() }));
            validate_workspace(&path).map(Some)
        })
    }

    /// Blocking; selects and reads one bounded regular JSON file chosen locally.
    pub fn open_json_file(limit: usize) -> Result<Option<Vec<u8>>, String> {
        run_sta(move || {
            let dialog: IFileOpenDialog = unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| format!("Could not create the JSON file picker: {error}"))?;
            let options = unsafe { dialog.GetOptions() }
                .map_err(|error| format!("Could not read JSON picker options: {error}"))?;
            unsafe { dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_NOCHANGEDIR) }
                .map_err(|error| format!("Could not configure the JSON picker: {error}"))?;
            let name = w!("JSON files");
            let pattern = w!("*.json");
            let filters = [COMDLG_FILTERSPEC { pszName: name, pszSpec: pattern }];
            unsafe { dialog.SetFileTypes(&filters) }
                .map_err(|error| format!("Could not set JSON picker file types: {error}"))?;
            unsafe { dialog.SetTitle(w!("Import typed macro JSON")) }
                .map_err(|error| format!("Could not set the JSON picker title: {error}"))?;
            match unsafe { dialog.Show(None::<HWND>) } {
                Ok(()) => {}
                Err(error) if dialog_cancelled(&error) => return Ok(None),
                Err(error) => return Err(format!("Could not show the JSON picker: {error}")),
            }
            let item = unsafe { dialog.GetResult() }
                .map_err(|error| format!("Could not read the selected JSON file: {error}"))?;
            let name = TaskMem(
                unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
                    .map_err(|error| format!("The selected JSON has no filesystem path: {error}"))?
                    .0,
            );
            if name.0.is_null() {
                return Err("The selected JSON has no filesystem path.".into());
            }
            let path = PathBuf::from(OsString::from_wide(unsafe { PWSTR(name.0).as_wide() }));
            let metadata = path.symlink_metadata().map_err(|error| format!("Could not inspect the selected JSON: {error}"))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > limit as u64 {
                return Err(format!("Selected JSON must be a regular file no larger than {limit} bytes."));
            }
            let mut bytes = Vec::new();
            use std::io::Read as _;
            std::fs::File::open(&path)
                .and_then(|file| file.take(limit as u64 + 1).read_to_end(&mut bytes))
                .map_err(|error| format!("Could not read the selected JSON: {error}"))?;
            if bytes.len() > limit {
                return Err(format!("Selected JSON exceeds {limit} bytes."));
            }
            Ok(Some(bytes))
        })
    }

    pub fn save_json_file(text: &str, default_name: &str) -> Result<Option<PathBuf>, String> {
        let text = text.to_string();
        let default_name = default_name.to_string();
        run_sta(move || {
            let dialog: IFileSaveDialog = unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| format!("Could not create the JSON save dialog: {error}"))?;
            let options = unsafe { dialog.GetOptions() }
                .map_err(|error| format!("Could not read JSON save options: {error}"))?;
            unsafe { dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_OVERWRITEPROMPT | FOS_NOCHANGEDIR) }
                .map_err(|error| format!("Could not configure the JSON save dialog: {error}"))?;
            unsafe { dialog.SetTitle(w!("Save JSON")) }.map_err(|error| format!("Could not set JSON save title: {error}"))?;
            let default = wide_null(OsStr::new(&default_name))?;
            unsafe { dialog.SetFileName(PCWSTR(default.as_ptr())) }.map_err(|error| format!("Could not set JSON file name: {error}"))?;
            unsafe { dialog.SetDefaultExtension(w!("json")) }.map_err(|error| format!("Could not set JSON extension: {error}"))?;
            match unsafe { dialog.Show(None::<HWND>) } {
                Ok(()) => {}
                Err(error) if dialog_cancelled(&error) => return Ok(None),
                Err(error) => return Err(format!("Could not show the JSON save dialog: {error}")),
            }
            let item = unsafe { dialog.GetResult() }.map_err(|error| format!("Could not read the JSON target: {error}"))?;
            let name = TaskMem(unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.map_err(|error| format!("The JSON target has no filesystem path: {error}"))?.0);
            if name.0.is_null() { return Err("The JSON target has no filesystem path.".into()); }
            let path = PathBuf::from(OsString::from_wide(unsafe { PWSTR(name.0).as_wide() }));
            stage_text_swap(&path, &text)?;
            Ok(Some(path))
        })
    }

    /// Blocking; shows a native Save dialog, then writes the text to the chosen file.
    /// Once the desktop event loop is running, call from a worker thread. None means the
    /// user cancelled, and a failed write leaves any existing target file untouched.
    pub fn save_text_file(text: &str) -> Result<Option<PathBuf>, String> {
        let text = text.to_string();
        run_sta(move || {
            let dialog: IFileSaveDialog = unsafe { CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER) }
                .map_err(|error| format!("Could not create the save dialog: {error}"))?;
            let options = unsafe { dialog.GetOptions() }
                .map_err(|error| format!("Could not read save dialog options: {error}"))?;
            unsafe {
                dialog.SetOptions(
                    options | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_OVERWRITEPROMPT | FOS_NOCHANGEDIR,
                )
            }
            .map_err(|error| format!("Could not configure the save dialog: {error}"))?;
            unsafe { dialog.SetTitle(w!("Save output")) }
                .map_err(|error| format!("Could not set the save dialog title: {error}"))?;
            unsafe { dialog.SetFileName(w!("chasm-output.txt")) }
                .map_err(|error| format!("Could not set the save dialog default name: {error}"))?;
            unsafe { dialog.SetDefaultExtension(w!("txt")) }
                .map_err(|error| format!("Could not set the save dialog default type: {error}"))?;
            match unsafe { dialog.Show(None::<HWND>) } {
                Ok(()) => {}
                Err(error) if dialog_cancelled(&error) => return Ok(None),
                Err(error) => return Err(format!("Could not show the save dialog: {error}")),
            }
            let item =
                unsafe { dialog.GetResult() }.map_err(|error| format!("Could not read the selected file: {error}"))?;
            let name = TaskMem(
                unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }
                    .map_err(|error| format!("The selected file has no filesystem path: {error}"))?
                    .0,
            );
            if name.0.is_null() {
                return Err("The selected file has no filesystem path.".into());
            }
            let path = PathBuf::from(OsString::from_wide(unsafe { PWSTR(name.0).as_wide() }));
            stage_text_swap(&path, &text)?;
            Ok(Some(path))
        })
    }

    fn dialog_cancelled(error: &windows::core::Error) -> bool {
        error.code() == HRESULT::from_win32(ERROR_CANCELLED.0)
    }

    /// Writes `text` through a sibling temp file so the full bytes are on disk before the
    /// swap: ReplaceFileW replaces an existing target without truncating it and keeps the
    /// old file on any failure, and a missing target falls back to a plain rename.
    fn stage_text_swap(target: &Path, text: &str) -> Result<(), String> {
        let parent = target.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        let mut staged = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| format!("Could not create the staged output file: {error}"))?;
        use std::io::Write as _;
        staged
            .write_all(text.as_bytes())
            .map_err(|error| format!("Could not write the staged output file: {error}"))?;
        staged.as_file().sync_all().map_err(|error| format!("Could not flush the staged output file: {error}"))?;
        let mut staged = staged.into_temp_path();
        use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
        use windows_sys::Win32::Storage::FileSystem::{ReplaceFileW, SetFileAttributesW, FILE_ATTRIBUTE_NORMAL};
        let target_name = wide_null(target.as_os_str())?;
        let staging_name = wide_null(staged.as_os_str())?;
        // Both NUL-terminated path buffers remain valid throughout these calls.
        unsafe {
            if SetFileAttributesW(staging_name.as_ptr(), FILE_ATTRIBUTE_NORMAL) == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            if ReplaceFileW(
                target_name.as_ptr(),
                staging_name.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            ) != 0
            {
                staged.disable_cleanup(true);
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_FILE_NOT_FOUND as i32) {
                return Err(format!("Could not replace the existing file: {error}"));
            }
        }
        staged.persist(target).map(|_| ()).map_err(|error| format!("Could not create the output file: {error}"))
    }

    /// Blocking; opens only the directory, with no executable or command-line parameters.
    pub fn open_folder(path: &Path) -> Result<(), String> {
        let path = path.to_path_buf();
        run_sta(move || {
            let path = shell_path(&validate_workspace(&path)?)?;
            let result = unsafe {
                ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL)
            };
            let code = result.0 as usize;
            if code <= 32 {
                return Err(format!("Could not open the folder (Windows shell error {code})."));
            }
            Ok(())
        })
    }

    /// Blocking; selects the named entry in Explorer without opening its contents.
    pub fn reveal_file(path: &Path) -> Result<(), String> {
        let path = path.to_path_buf();
        run_sta(move || {
            let name = path.file_name().ok_or("Choose a file to show in its folder.")?;
            let parent =
                path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
            // Resolve the parent, not a symlink's target, so Explorer selects the named entry.
            let path = validate_workspace(parent)?.join(name);
            let metadata =
                path.symlink_metadata().map_err(|error| format!("Could not inspect the file to show: {error}"))?;
            if !metadata.is_file() && !metadata.file_type().is_symlink() {
                return Err("The path to show must be a file.".into());
            }
            let path = shell_path(&path)?;
            let mut item = TaskMem::<ITEMIDLIST>(std::ptr::null_mut());
            unsafe { SHParseDisplayName(PCWSTR(path.as_ptr()), None, &mut item.0, 0, None) }
                .map_err(|error| format!("Could not resolve the file in Windows Explorer: {error}"))?;
            if item.0.is_null() {
                return Err("Windows Explorer returned no file to select.".into());
            }
            // With no child list, the absolute PIDL identifies the single item to select.
            unsafe { SHOpenFolderAndSelectItems(item.0, None, 0) }
                .map_err(|error| format!("Could not show the file in its folder: {error}"))
        })
    }

    pub fn error_message(title: &str, message: &str) -> Result<(), String> {
        let title = wide_null(OsStr::new(title))?;
        let message = wide_null(OsStr::new(message))?;
        let result = unsafe {
            MessageBoxW(
                None,
                PCWSTR(message.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR | MB_TASKMODAL | MB_SETFOREGROUND,
            )
        };
        if result.0 == 0 {
            return Err(format!("Could not show the error dialog: {}", std::io::Error::last_os_error()));
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{dialog_cancelled, shell_path, stage_text_swap, wide_null};
        use std::ffi::{OsStr, OsString};
        use std::os::windows::ffi::OsStringExt;
        use std::os::windows::fs::OpenOptionsExt;
        use std::path::Path;
        use windows::{
            core::{Error, HRESULT},
            Win32::Foundation::ERROR_CANCELLED,
        };

        #[test]
        fn windows_text_preserves_spaces_commas_and_unicode() {
            let text = r"C:\work space, comma\项目 λ 𐐀";
            let wide = wide_null(OsStr::new(text)).unwrap();
            assert_eq!(wide, text.encode_utf16().chain(Some(0)).collect::<Vec<_>>());
            assert_eq!(OsString::from_wide(&wide[..wide.len() - 1]), OsStr::new(text));
            assert_eq!(wide_null(OsStr::new("")).unwrap(), vec![0]);
        }

        #[test]
        fn windows_text_preserves_unpaired_surrogates() {
            let text = OsString::from_wide(&[b'a' as u16, 0xd800, b'b' as u16]);
            assert_eq!(wide_null(&text).unwrap(), vec![b'a' as u16, 0xd800, b'b' as u16, 0]);
        }

        #[test]
        fn windows_text_rejects_interior_nul() {
            assert!(wide_null(OsStr::new("before\0after")).is_err());
        }

        #[test]
        fn explorer_paths_remove_only_verbatim_drive_or_unc_prefixes() {
            for (input, expected) in [
                (r"\\?\C:\work space, comma\项目 λ 𐐀", r"C:\work space, comma\项目 λ 𐐀"),
                (r"\\?\UNC\server\share\work space, 项目", r"\\server\share\work space, 项目"),
                (r"C:\work space, comma\项目", r"C:\work space, comma\项目"),
            ] {
                assert_eq!(shell_path(Path::new(input)).unwrap(), wide_null(OsStr::new(expected)).unwrap());
            }
        }

        #[test]
        fn explorer_paths_reject_device_namespaces_and_lossy_components() {
            for path in [r"\\.\PhysicalDrive0", r"\\?\GLOBALROOT\Device", r"\\?\C:\workspace.", r"\\?\C:\workspace "] {
                assert!(shell_path(Path::new(path)).is_err());
            }
        }

        #[test]
        fn staged_swap_writes_a_new_file_without_leaving_temporaries() {
            let fixture = tempfile::tempdir().unwrap();
            let target = fixture.path().join("output dir, 项目").join("chasm-output.txt");
            std::fs::create_dir(target.parent().unwrap()).unwrap();
            stage_text_swap(&target, "first\n").unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "first\n");
            assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);
        }

        #[test]
        fn staged_swap_replaces_an_existing_file_in_place() {
            let fixture = tempfile::tempdir().unwrap();
            let target = fixture.path().join("chasm-output.txt");
            std::fs::write(&target, "old").unwrap();
            stage_text_swap(&target, "new").unwrap();
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
            assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);
        }

        #[test]
        fn staged_swap_failure_keeps_the_existing_file_and_cleans_the_staging() {
            let fixture = tempfile::tempdir().unwrap();
            let target = fixture.path().join("chasm-output.txt");
            std::fs::write(&target, "old").unwrap();
            // Locking the target makes ReplaceFileW fail without touching it.
            let lock = std::fs::OpenOptions::new().read(true).write(true).share_mode(0).open(&target).unwrap();
            assert!(stage_text_swap(&target, "new").is_err());
            drop(lock);
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
            assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 1);
        }

        #[test]
        fn save_dialog_cancellation_is_distinguished_from_other_errors() {
            let cancelled = Error::from_hresult(HRESULT::from_win32(ERROR_CANCELLED.0));
            assert!(dialog_cancelled(&cancelled));
            let failure = Error::from_hresult(HRESULT(0x8000_4005u32 as i32));
            assert!(!dialog_cancelled(&failure));
        }
    }
}

#[cfg(not(windows))]
mod native {
    use std::path::{Path, PathBuf};

    pub fn pick_folder_with_owner(_: Option<&Path>, _: Option<isize>) -> Result<Option<PathBuf>, String> {
        Err("The native folder picker is only supported on Windows; provide a workspace path explicitly.".into())
    }

    pub fn open_json_file(_: usize) -> Result<Option<Vec<u8>>, String> {
        Err("Opening JSON files from the desktop is only supported on Windows.".into())
    }

    pub fn save_json_file(_: &str, _: &str) -> Result<Option<PathBuf>, String> {
        Err("Saving JSON files from the desktop is only supported on Windows.".into())
    }

    pub fn save_text_file(_: &str) -> Result<Option<PathBuf>, String> {
        Err("Saving files from the desktop is only supported on Windows.".into())
    }

    pub fn open_folder(_: &Path) -> Result<(), String> {
        Err("Opening folders from the desktop is only supported on Windows.".into())
    }

    pub fn reveal_file(_: &Path) -> Result<(), String> {
        Err("Showing files in their folder is only supported on Windows.".into())
    }

    pub fn error_message(_: &str, _: &str) -> Result<(), String> {
        Err("Native error dialogs are only supported on Windows.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::validate_workspace;
    use std::path::Path;

    #[test]
    fn workspace_accepts_unicode_spaces_and_commas_without_writing_settings() {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("work space, 项目 λ 𐐀");
        std::fs::create_dir(&path).unwrap();
        assert_eq!(validate_workspace(&path).unwrap(), path.canonicalize().unwrap());
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    }

    #[test]
    fn workspace_aliases_resolve_to_the_same_root() {
        let fixture = tempfile::tempdir().unwrap();
        let child = fixture.path().join("child");
        std::fs::create_dir(&child).unwrap();
        assert_eq!(validate_workspace(&child.join("..")).unwrap(), validate_workspace(fixture.path()).unwrap());
    }

    #[test]
    fn workspace_rejects_missing_paths_without_creating_them() {
        let fixture = tempfile::tempdir().unwrap();
        let missing = fixture.path().join("missing");
        assert!(validate_workspace(&missing).is_err());
        assert!(!missing.exists());
        assert_eq!(std::fs::read_dir(fixture.path()).unwrap().count(), 0);
    }

    #[test]
    fn workspace_rejects_regular_files() {
        let fixture = tempfile::tempdir().unwrap();
        let file = tempfile::NamedTempFile::new_in(fixture.path()).unwrap();
        assert!(validate_workspace(file.path()).is_err());
    }

    #[test]
    fn workspace_rejects_an_empty_path() {
        assert!(validate_workspace(Path::new("")).is_err());
    }
}
