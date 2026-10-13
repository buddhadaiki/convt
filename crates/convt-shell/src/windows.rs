use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{OsString, c_void};
use std::io::Read;
use std::os::windows::{ffi::OsStringExt, process::CommandExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Ole::*;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Shell::*;
use windows::core::*;

pub const CLSID: GUID = GUID::from_u128(0x710fb9a8_c47e_4b39_9cfa_e273ab1b78f8);
const MORE_CANONICAL_NAME: GUID = GUID::from_u128(0x91ddb858_d7e3_4b6b_98bd_97608fb90226);
static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static SERVER_LOCKS: AtomicUsize = AtomicUsize::new(0);
struct ModuleLease;
impl ModuleLease {
    fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}
impl Drop for ModuleLease {
    fn drop(&mut self) {
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}
const PROBE_LIMIT: Duration = Duration::from_secs(2);

fn error() -> Error {
    Error::from_hresult(E_FAIL)
}
fn text(value: &str) -> Result<PWSTR> {
    let wide = HSTRING::from(value);
    // Shell owns the CoTaskMem string returned by SHStrDupW.
    unsafe { SHStrDupW(&wide) }
}
fn install_dir() -> Result<PathBuf> {
    let mut module = HMODULE::default();
    let mut buffer = vec![0u16; 32768];
    // Resolve this DLL, not Explorer.exe/dllhost.exe and never PATH.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(DllGetClassObject as *const () as *const u16),
            &mut module,
        )?;
        let len = GetModuleFileNameW(Some(module), &mut buffer) as usize;
        if len == 0 || len >= buffer.len() {
            return Err(error());
        }
        PathBuf::from(OsString::from_wide(&buffer[..len]))
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(error)
    }
}
fn files(items: Ref<IShellItemArray>) -> Result<Vec<PathBuf>> {
    let items = items.ok()?;
    let mut paths = Vec::new();
    unsafe {
        let count = items.GetCount()?;
        if count == 0 || count > 256 {
            return Err(error());
        }
        for index in 0..count {
            let item = items.GetItemAt(index)?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH)?;
            let path = PathBuf::from(OsString::from_wide(name.as_wide()));
            CoTaskMemFree(Some(name.0.cast()));
            if !path.is_absolute() || !path.is_file() {
                return Err(error());
            }
            paths.push(path);
        }
    }
    Ok(paths)
}

// Deduplicate only within this selection. A later menu construction must read
// current preferences and engine availability through the CLI again.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MenuSelection {
    targets: Vec<String>,
    more: bool,
}

fn selection_menu_with(
    paths: &[PathBuf],
    mut query: impl FnMut(&Path) -> Option<MenuSelection>,
) -> MenuSelection {
    if paths.is_empty() {
        return MenuSelection::default();
    }
    let mut by_extension = HashMap::<OsString, MenuSelection>::new();
    let mut lists = Vec::new();
    for path in paths {
        let Some(extension) = path.extension().map(|e| e.to_ascii_lowercase()) else {
            return MenuSelection::default();
        };
        let menu = by_extension
            .entry(extension)
            .or_insert_with(|| query(path).unwrap_or_default());
        // CLI uses more=false for disabled or unrecognized inputs. A mixed
        // selection is offered only when every input can open Quick convert.
        if !menu.more {
            return MenuSelection::default();
        }
        lists.push(menu.targets.clone());
    }
    MenuSelection {
        targets: crate::common_targets(&lists),
        more: true,
    }
}

fn selection_menu(paths: &[PathBuf]) -> MenuSelection {
    selection_menu_with(paths, probe)
}

fn parse_probe(bytes: &[u8]) -> Option<MenuSelection> {
    if bytes.len() > 4096 {
        return None;
    }
    let menu: MenuSelection = serde_json::from_slice(bytes).ok()?;
    let mut seen = std::collections::HashSet::new();
    if menu.targets.iter().any(|id| {
        id.is_empty()
            || id.len() > 32
            || matches!(id.as_str(), "mais" | "more")
            || !id
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            || !seen.insert(id)
    }) || (!menu.more && !menu.targets.is_empty())
    {
        return None;
    }
    Some(menu)
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CommandMode {
    Root,
    Format(String),
    More,
}

impl CommandMode {
    fn canonical_name(&self) -> GUID {
        match self {
            Self::Root => CLSID,
            Self::More => MORE_CANONICAL_NAME,
            Self::Format(target) => {
                let mut id = CLSID;
                // Stable names for formats remain independent of menu order.
                id.data1 ^= target
                    .bytes()
                    .fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
                id
            }
        }
    }
}

fn command_modes(menu: MenuSelection) -> Vec<CommandMode> {
    if !menu.more {
        return Vec::new();
    }
    let mut modes: Vec<_> = menu.targets.into_iter().map(CommandMode::Format).collect();
    modes.push(CommandMode::More);
    modes
}

#[derive(Serialize)]
struct ExplorerRequest<'a> {
    files: &'a [PathBuf],
    show_progress: bool,
    to: Option<&'a str>,
    preset: Option<&'a str>,
    source: &'static str,
    license: Option<&'static str>,
    auth: Option<()>,
}

fn request_for<'a>(paths: &'a [PathBuf], mode: &'a CommandMode) -> Result<ExplorerRequest<'a>> {
    let to = match mode {
        CommandMode::Root => return Err(error()),
        CommandMode::Format(target) => Some(target.as_str()),
        CommandMode::More => None,
    };
    Ok(ExplorerRequest {
        files: paths,
        show_progress: to.is_some(),
        to,
        preset: None,
        source: "Cli",
        license: None,
        auth: None,
    })
}

fn request_dir() -> PathBuf {
    std::env::var_os("CONVT_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("convt"))
}

fn handoff(paths: &[PathBuf], mode: &CommandMode) -> Result<()> {
    static REQUEST_ID: AtomicUsize = AtomicUsize::new(0);
    let request = request_for(paths, mode)?;
    let dir = request_dir();
    std::fs::create_dir_all(&dir).map_err(|_| error())?;
    let id = format!(
        "{}-{}",
        std::process::id(),
        REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    );
    let bytes = serde_json::to_vec(&request).map_err(|_| error())?;
    let temp = dir.join(format!("request-{id}.tmp"));
    let path = dir.join(format!("request-{id}.json"));
    std::fs::write(&temp, bytes).map_err(|_| error())?;
    std::fs::rename(temp, path).map_err(|_| error())?;
    Ok(())
}

fn app_running() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    let runtime_override = std::env::var_os("CONVT_RUNTIME_DIR").map(PathBuf::from);
    let name: Vec<u16> = std::ffi::OsStr::new(&mutex_name(runtime_override.as_deref()))
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: valid NUL-terminated mutex name; the handle is closed below.
    let Ok(handle) =
        (unsafe { OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) })
    else {
        return false;
    };
    // SAFETY: the handle came from OpenMutexW above and is closed once.
    let _ = unsafe { CloseHandle(handle) };
    true
}

/// Must match the app's instance key so an isolated runtime never forwards
/// requests to a user's ordinary app. No override keeps the installed name.
fn mutex_name(runtime_override: Option<&Path>) -> String {
    use std::os::windows::ffi::OsStrExt;
    match runtime_override {
        None => "Local\\convt-instance".into(),
        Some(dir) => {
            let key = dir
                .as_os_str()
                .encode_wide()
                .fold(14695981039346656037u64, |key, unit| {
                    (key ^ u64::from(unit)).wrapping_mul(1099511628211)
                });
            format!("Local\\convt-instance-{key:016x}")
        }
    }
}

fn app_launch(app_path: &Path) -> Command {
    let mut command = Command::new(app_path);
    command
        .arg("--from-explorer")
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}
fn probe(path: &Path) -> Option<MenuSelection> {
    let mut child = Command::new(install_dir().ok()?.join("convt.exe"))
        .arg("targets")
        .arg(path)
        .arg("--menu")
        .arg("--json")
        .creation_flags(CREATE_NO_WINDOW.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(4097).read_to_end(&mut bytes).ok()?;
        (bytes.len() <= 4096).then_some(bytes)
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < PROBE_LIMIT => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().ok()??;
    if !status?.success() {
        return None;
    }
    parse_probe(&bytes)
}

#[implement(IExplorerCommand, IObjectWithSite)]
struct ExplorerCommand {
    _lease: ModuleLease,
    mode: CommandMode,
    site: Mutex<Option<IUnknown>>,
    selection: Mutex<Vec<PathBuf>>,
}
impl ExplorerCommand {
    fn root() -> Self {
        Self {
            _lease: ModuleLease::new(),
            mode: CommandMode::Root,
            site: Mutex::new(None),
            selection: Mutex::new(vec![]),
        }
    }
}
impl IObjectWithSite_Impl for ExplorerCommand_Impl {
    fn SetSite(&self, site: Ref<IUnknown>) -> Result<()> {
        *self.site.lock().map_err(|_| error())? = site.cloned();
        Ok(())
    }
    fn GetSite(&self, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if iid.is_null() || out.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        let site = self.site.lock().map_err(|_| error())?;
        unsafe { site.as_ref().ok_or_else(error)?.query(iid, out).ok() }
    }
}
impl ExplorerCommand_Impl {
    fn selected_paths(&self) -> Result<Vec<PathBuf>> {
        let paths = self.selection.lock().map_err(|_| error())?.clone();
        if !paths.is_empty() {
            return Ok(paths);
        }
        // Some shell hosts enumerate before passing an item array to GetTitle
        // or GetState. Query the site's current view only during construction;
        // Invoke always uses the array supplied by the shell or this snapshot.
        let site = self
            .site
            .lock()
            .map_err(|_| error())?
            .clone()
            .ok_or_else(error)?;
        let services: IServiceProvider = site.cast()?;
        unsafe {
            let browser: IShellBrowser = services.QueryService(&SID_STopLevelBrowser)?;
            let view = browser.QueryActiveShellView()?;
            let items: IShellItemArray = view.GetItemObject(SVGIO_SELECTION)?;
            let paths = files((&items).into())?;
            *self.selection.lock().map_err(|_| error())? = paths.clone();
            Ok(paths)
        }
    }
}
impl IExplorerCommand_Impl for ExplorerCommand_Impl {
    fn GetTitle(&self, items: Ref<IShellItemArray>) -> Result<PWSTR> {
        if self.mode == CommandMode::Root
            && let Ok(paths) = files(items)
        {
            *self.selection.lock().map_err(|_| error())? = paths;
        }
        text(&match &self.mode {
            CommandMode::Root => "Convert with convt".into(),
            CommandMode::Format(target) => target.to_uppercase(),
            CommandMode::More => "Mais".into(),
        })
    }
    fn GetIcon(&self, _: Ref<IShellItemArray>) -> Result<PWSTR> {
        text(&format!(
            "{},0",
            install_dir()?.join("convt-app.exe").display()
        ))
    }
    fn GetToolTip(&self, _: Ref<IShellItemArray>) -> Result<PWSTR> {
        text("Convert locally with convt")
    }
    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(self.mode.canonical_name())
    }
    fn GetState(&self, items: Ref<IShellItemArray>, slow: BOOL) -> Result<u32> {
        if self.mode != CommandMode::Root {
            return Ok(ECS_ENABLED.0 as u32);
        }
        if !slow.as_bool() {
            return Err(Error::from_hresult(HRESULT(0x8000000Au32 as i32)));
        }
        let Ok(paths) = files(items) else {
            return Ok(ECS_HIDDEN.0 as u32);
        };
        let menu = selection_menu(&paths);
        *self.selection.lock().map_err(|_| error())? = paths;
        let state = if menu.more { ECS_ENABLED } else { ECS_HIDDEN };
        Ok(state.0 as u32)
    }
    fn Invoke(&self, items: Ref<IShellItemArray>, _: Ref<IBindCtx>) -> Result<()> {
        if self.mode == CommandMode::Root {
            return Err(error());
        }
        let paths = if items.is_some() {
            files(items)?
        } else {
            self.selection.lock().map_err(|_| error())?.clone()
        };
        if paths.is_empty() {
            return Err(error());
        }
        handoff(&paths, &self.mode)?;
        if !app_running() {
            app_launch(&install_dir()?.join("convt-app.exe"))
                .spawn()
                .map_err(|_| error())?;
        }
        Ok(())
    }
    fn GetFlags(&self) -> Result<u32> {
        Ok(if self.mode == CommandMode::Root {
            ECF_HASSUBCOMMANDS.0 as u32
        } else {
            0
        })
    }
    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        if self.mode != CommandMode::Root {
            return Err(Error::from_hresult(E_NOTIMPL));
        }
        let paths = self.selected_paths()?;
        let commands = command_modes(selection_menu(&paths))
            .into_iter()
            .map(|mode| {
                ExplorerCommand {
                    _lease: ModuleLease::new(),
                    mode,
                    site: Mutex::new(None),
                    selection: Mutex::new(paths.clone()),
                }
                .into()
            })
            .collect();
        Ok(Enumerator {
            _lease: ModuleLease::new(),
            commands,
            cursor: Mutex::new(0),
        }
        .into())
    }
}

#[implement(IEnumExplorerCommand)]
struct Enumerator {
    _lease: ModuleLease,
    commands: Vec<IExplorerCommand>,
    cursor: Mutex<usize>,
}
impl IEnumExplorerCommand_Impl for Enumerator_Impl {
    fn Next(
        &self,
        count: u32,
        commands: *mut Option<IExplorerCommand>,
        fetched: *mut u32,
    ) -> HRESULT {
        if commands.is_null() || (fetched.is_null() && count != 1) {
            return E_POINTER;
        }
        let Ok(mut cursor) = self.cursor.lock() else {
            return E_FAIL;
        };
        let take = (count as usize).min(self.commands.len() - *cursor);
        // COM caller provides count output slots. Each receives a new reference.
        unsafe {
            if !fetched.is_null() {
                *fetched = take as u32;
            }
            for i in 0..count as usize {
                commands.add(i).write(if i < take {
                    Some(self.commands[*cursor + i].clone())
                } else {
                    None
                });
            }
        }
        *cursor += take;
        if take == count as usize {
            S_OK
        } else {
            S_FALSE
        }
    }
    fn Skip(&self, count: u32) -> Result<()> {
        let mut cursor = self.cursor.lock().map_err(|_| error())?;
        let remaining = self.commands.len() - *cursor;
        *cursor += (count as usize).min(remaining);
        if count as usize > remaining {
            // The projection uses Result even though COM permits S_FALSE here.
            Err(Error::from_hresult(S_FALSE))
        } else {
            Ok(())
        }
    }
    fn Reset(&self) -> Result<()> {
        *self.cursor.lock().map_err(|_| error())? = 0;
        Ok(())
    }
    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        Ok(Enumerator {
            _lease: ModuleLease::new(),
            commands: self.commands.clone(),
            cursor: Mutex::new(*self.cursor.lock().map_err(|_| error())?),
        }
        .into())
    }
}

#[implement(IClassFactory)]
struct Factory {
    _lease: ModuleLease,
}
impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        outer: Ref<IUnknown>,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> Result<()> {
        if out.is_null() || iid.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        unsafe {
            *out = std::ptr::null_mut();
        }
        if outer.is_some() {
            return Err(Error::from_hresult(CLASS_E_NOAGGREGATION));
        }
        let command: IExplorerCommand = ExplorerCommand::root().into();
        unsafe { command.query(iid, out).ok() }
    }
    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            SERVER_LOCKS.fetch_add(1, Ordering::SeqCst);
        } else {
            let _ = SERVER_LOCKS.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                count.checked_sub(1)
            });
        }
        Ok(())
    }
}

/// COM export; Windows supplies valid GUIDs and an output pointer.
#[unsafe(no_mangle)]
unsafe extern "system" fn DllGetClassObject(
    class: *const GUID,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if out.is_null() || class.is_null() || iid.is_null() {
        return E_POINTER;
    }
    unsafe {
        *out = std::ptr::null_mut();
        if *class != CLSID {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory {
            _lease: ModuleLease::new(),
        }
        .into();
        factory.query(iid, out)
    }
}
// COM may release the DLL only after every object and server lock is gone.
// Probe workers join before the owning command returns.
#[unsafe(no_mangle)]
extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 && SERVER_LOCKS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_override_uses_the_same_stable_namespace_as_the_app() {
        assert_eq!(mutex_name(None), "Local\\convt-instance");
        assert_eq!(
            mutex_name(Some(Path::new(r"C:\convt-runtime-ação"))),
            "Local\\convt-instance-bafbcab710422a4c"
        );
        assert_ne!(
            mutex_name(Some(Path::new(r"C:\one"))),
            mutex_name(Some(Path::new(r"C:\two")))
        );
    }

    #[test]
    fn explorer_starts_only_the_inbox_listener_with_a_bounded_command_line() {
        let app = Path::new(r"C:\Program Files\convt\convt-app.exe");
        let command = app_launch(app);
        assert_eq!(command.get_program(), app.as_os_str());
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["--from-explorer"]);
    }

    fn menu(targets: &[&str]) -> MenuSelection {
        MenuSelection {
            targets: targets.iter().map(|target| (*target).into()).collect(),
            more: true,
        }
    }

    #[test]
    fn selection_probes_each_extension_once_and_preserves_intersection_order() {
        let paths = ["one.png", "two.PNG", "three.jpg"].map(PathBuf::from);
        let mut calls = Vec::new();
        let result = selection_menu_with(&paths, |path| {
            calls.push(path.to_path_buf());
            Some(if path.extension().unwrap() == "jpg" {
                menu(&["pdf", "webp"])
            } else {
                menu(&["webp", "jpeg", "pdf"])
            })
        });
        assert_eq!(calls, [paths[0].clone(), paths[2].clone()]);
        assert_eq!(result, menu(&["webp", "pdf"]));
    }

    #[test]
    fn later_selection_reads_new_preferences_without_a_cache_delay() {
        let paths = [PathBuf::from("one.png")];
        let first = selection_menu_with(&paths, |_| Some(menu(&["jpeg"])));
        let changed = selection_menu_with(&paths, |_| Some(menu(&["webp"])));
        let custom_empty = selection_menu_with(&paths, |_| Some(menu(&[])));
        let disabled = selection_menu_with(&paths, |_| Some(MenuSelection::default()));
        assert_eq!(first, menu(&["jpeg"]));
        assert_eq!(changed, menu(&["webp"]));
        assert_eq!(command_modes(custom_empty), [CommandMode::More]);
        assert_eq!(disabled, MenuSelection::default());
    }

    #[test]
    fn failed_or_extensionless_probes_hide_the_menu() {
        let paths = ["one.png", "two.png", "unknown"].map(PathBuf::from);
        let mut calls = 0;
        assert_eq!(
            selection_menu_with(&paths, |_| {
                calls += 1;
                None
            }),
            MenuSelection::default()
        );
        assert_eq!(calls, 1);
        assert_eq!(
            selection_menu_with(&paths, |_| Some(menu(&["jpeg"]))),
            MenuSelection::default()
        );
        assert_eq!(
            selection_menu_with(&[], |_| unreachable!()),
            MenuSelection::default()
        );
    }

    #[test]
    fn mixed_selection_keeps_more_only_when_every_input_is_recognized_and_enabled() {
        let paths = ["one.png", "two.mp4"].map(PathBuf::from);
        let mixed = selection_menu_with(&paths, |path| {
            Some(if path.extension().unwrap() == "png" {
                menu(&["jpeg", "webp"])
            } else {
                menu(&["mkv", "gif"])
            })
        });
        assert_eq!(command_modes(mixed), [CommandMode::More]);
        for unavailable in [None, Some(MenuSelection::default())] {
            assert_eq!(
                selection_menu_with(&paths, |path| {
                    if path.extension().unwrap() == "png" {
                        Some(menu(&["jpeg"]))
                    } else {
                        unavailable.clone()
                    }
                }),
                MenuSelection::default()
            );
        }
    }

    #[test]
    fn probe_json_requires_the_complete_bounded_contract_and_valid_target_ids() {
        assert_eq!(
            parse_probe(br#"{"targets":["jpeg","webp"],"more":true}"#),
            Some(menu(&["jpeg", "webp"]))
        );
        assert_eq!(
            parse_probe(br#"{"targets":[],"more":true}"#),
            Some(menu(&[]))
        );
        assert_eq!(
            parse_probe(br#"{"targets":[],"more":false}"#),
            Some(MenuSelection::default())
        );
        let invalid: &[&[u8]] = &[
            b"jpeg webp",
            br#"{"targets":[]}"#,
            br#"{"more":true}"#,
            br#"{"targets":[],"more":"true"}"#,
            br#"{"targets":[],"more":true,"extra":1}"#,
            br#"{"targets":[],"more":true,"more":true}"#,
            br#"{"targets":["jpeg"],"more":true} {}"#,
            br#"{"targets":["jpeg"],"more":false}"#,
            br#"{"targets":[""],"more":true}"#,
            br#"{"targets":["JPEG"],"more":true}"#,
            br#"{"targets":["jpeg","jpeg"],"more":true}"#,
            br#"{"targets":["mais"],"more":true}"#,
            br#"{"targets":["more"],"more":true}"#,
        ];
        for bytes in invalid {
            assert!(
                parse_probe(bytes).is_none(),
                "unexpected probe JSON: {bytes:?}"
            );
        }
        assert!(parse_probe(&vec![b' '; 4097]).is_none());
    }

    #[test]
    fn more_is_last_and_has_a_stable_canonical_name_outside_the_format_family() {
        let modes = command_modes(menu(&["webp", "jpeg"]));
        assert_eq!(
            modes,
            [
                CommandMode::Format("webp".into()),
                CommandMode::Format("jpeg".into()),
                CommandMode::More
            ]
        );
        let more = CommandMode::More.canonical_name();
        assert_eq!(more, MORE_CANONICAL_NAME);
        assert_ne!(more, CommandMode::Root.canonical_name());
        for target in ["jpeg", "webp", "mp4", "gif", "mp3"] {
            assert_ne!(more, CommandMode::Format(target.into()).canonical_name());
        }
        assert!(command_modes(MenuSelection::default()).is_empty());
    }

    #[test]
    fn more_request_opens_the_picker_while_formats_keep_auto_conversion_progress() {
        let paths = [PathBuf::from(r"C:\Files with spaces\ação & image.png")];
        let more = serde_json::to_value(request_for(&paths, &CommandMode::More).unwrap()).unwrap();
        assert!(more["to"].is_null());
        assert!(more["preset"].is_null());
        assert_eq!(more["show_progress"], false);
        assert_eq!(more["source"], "Cli");
        assert_eq!(more["files"][0], paths[0].to_str().unwrap());
        let format = CommandMode::Format("webp".into());
        let format = serde_json::to_value(request_for(&paths, &format).unwrap()).unwrap();
        assert_eq!(format["to"], "webp");
        assert_eq!(format["show_progress"], true);
        assert!(request_for(&paths, &CommandMode::Root).is_err());
    }
}
