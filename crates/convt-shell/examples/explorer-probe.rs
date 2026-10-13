//! Run on Windows against the installed DLL, without opening Explorer.

#[cfg(windows)]
fn capture_mutex_names(runtime_override: &std::ffi::OsStr) -> [String; 2] {
    use std::os::windows::ffi::OsStrExt;
    // Match the app and handler: hash the original UTF-16 environment value,
    // not the canonical inbox path (which may add a device prefix or resolve aliases).
    let key = runtime_override
        .encode_wide()
        .fold(14695981039346656037u64, |key, unit| {
            (key ^ u64::from(unit)).wrapping_mul(1099511628211)
        });
    [
        "Local\\convt-instance".into(),
        format!("Local\\convt-instance-{key:016x}"),
    ]
}

#[cfg(windows)]
fn require_idle_capture_runtime(
    runtime_override: &std::ffi::OsStr,
    mut mutex_exists: impl FnMut(&str) -> windows::core::Result<bool>,
) -> windows::core::Result<()> {
    for name in capture_mutex_names(runtime_override) {
        if mutex_exists(&name)? {
            return Err(windows::core::Error::new(
                windows::core::HRESULT::from_win32(
                    windows::Win32::Foundation::ERROR_ALREADY_EXISTS.0,
                ),
                "capture requires both the ordinary app and the isolated runtime app to be closed",
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn main() -> windows::core::Result<()> {
    use serde::Deserialize;
    use std::collections::HashMap;
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::PathBuf;
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND},
        System::{Com::*, LibraryLoader::*, Threading::*},
        UI::Shell::*,
    };
    use windows::core::*;
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let registered = args.first().is_some_and(|arg| arg == "--registered");
    let modern = args.first().is_some_and(|arg| arg == "--modern");
    if registered || modern {
        args.remove(0);
    }
    let capture_more = args.first().is_some_and(|arg| arg == "--capture-more");
    if capture_more {
        args.remove(0);
    }
    let refresh = if args.first().is_some_and(|arg| arg == "--refresh-settings") {
        args.remove(0);
        assert!(
            !args.is_empty(),
            "--refresh-settings requires a replacement TOML file"
        );
        Some(PathBuf::from(args.remove(0)))
    } else {
        None
    };
    assert!(args.len() >= 2, "explorer-probe <installed-DLL> <files...>");
    let paths: Vec<_> = args[1..].iter().map(PathBuf::from).collect();
    let capture = std::env::var_os("CONVT_SHELL_CAPTURE");
    assert!(
        !capture_more || capture.is_some(),
        "--capture-more requires an isolated CONVT_SHELL_CAPTURE fixture"
    );
    if let Some(capture) = &capture {
        assert!(
            !registered && !modern,
            "capture is allowed only with an isolated LoadLibrary fixture"
        );
        assert!(
            PathBuf::from(capture).is_absolute(),
            "capture output must be absolute"
        );
        let runtime_override = std::env::var_os("CONVT_RUNTIME_DIR")
            .expect("capture requires an isolated CONVT_RUNTIME_DIR");
        let runtime = PathBuf::from(&runtime_override);
        assert!(
            runtime.is_absolute() && runtime.is_dir(),
            "capture runtime must be an existing absolute directory"
        );
        let runtime = std::fs::canonicalize(runtime).unwrap();
        let normal_inbox = std::env::temp_dir().join("convt");
        let normal_inbox = std::fs::canonicalize(&normal_inbox).unwrap_or(normal_inbox);
        assert!(
            !runtime
                .to_string_lossy()
                .eq_ignore_ascii_case(&normal_inbox.to_string_lossy()),
            "capture must not use the app's normal runtime inbox"
        );
        // Invoke forwards to the running app instead of spawning the stub.
        // Check both namespaces before invoking: the handler can forward to
        // either a normal app or an app already using this runtime override.
        require_idle_capture_runtime(&runtime_override, |name| unsafe {
            let name: Vec<_> = std::ffi::OsStr::new(name)
                .encode_wide()
                .chain(Some(0))
                .collect();
            match OpenMutexW(SYNCHRONIZATION_SYNCHRONIZE, false, PCWSTR(name.as_ptr())) {
                Ok(handle) => {
                    let _ = CloseHandle(handle);
                    Ok(true)
                }
                Err(error) if error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => {
                    Ok(false)
                }
                Err(error) => Err(error),
            }
        })?;
        let dll = PathBuf::from(&args[0]);
        assert!(
            dll.is_absolute(),
            "capture DLL must be an absolute fixture path"
        );
        let dll = std::fs::canonicalize(dll).unwrap();
        // Validate the stub beside precisely the DLL that LoadLibrary will load.
        args[0] = dll.as_os_str().to_owned();
        let app = dll.parent().unwrap().join("convt-app.exe");
        assert!(
            std::fs::metadata(&app).unwrap().len() <= 8 * 1024 * 1024,
            "capture requires the small fixture app stub"
        );
        let bytes = std::fs::read(app).unwrap();
        let marker = b"CONVT_SHELL_CAPTURE";
        let wide: Vec<_> = "CONVT_SHELL_CAPTURE"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert!(
            bytes.windows(marker.len()).any(|bytes| bytes == marker)
                || bytes.windows(wide.len()).any(|bytes| bytes == wide),
            "capture app must be a fixture stub containing CONVT_SHELL_CAPTURE"
        );
    }
    #[derive(Clone, Default, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct MenuProbe {
        targets: Vec<String>,
        more: bool,
    }
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        let class = GUID::from_u128(0x710fb9a8_c47e_4b39_9cfa_e273ab1b78f8);
        let more_id = GUID::from_u128(0x91ddb858_d7e3_4b6b_98bd_97608fb90226);
        let root: IExplorerCommand = if modern {
            // Require the packaged COM surrogate: allowing INPROC here would
            // accidentally validate the classic registration instead.
            CoCreateInstance(&class, None, CLSCTX_LOCAL_SERVER)?
        } else if registered {
            // The per-user MSI registers the classic in-process COM handler.
            CoCreateInstance(&class, None, CLSCTX_INPROC_SERVER)?
        } else {
            let module = LoadLibraryW(&HSTRING::from(&args[0]))?;
            let proc = GetProcAddress(module, s!("DllGetClassObject")).expect("COM export");
            let get: unsafe extern "system" fn(
                *const GUID,
                *const GUID,
                *mut *mut c_void,
            ) -> HRESULT = std::mem::transmute(proc);
            let mut raw = std::ptr::null_mut();
            get(&class, &IClassFactory::IID, &mut raw).ok()?;
            let factory = IClassFactory::from_raw(raw);
            factory.CreateInstance(None)?
        };
        let mut pidls = Vec::new();
        for path in &paths {
            let mut pidl = std::ptr::null_mut();
            SHParseDisplayName(&HSTRING::from(path.as_os_str()), None, &mut pidl, 0, None)?;
            pidls.push(pidl);
        }
        let pointers: Vec<_> = pidls.iter().map(|p| *p as *const _).collect();
        let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&pointers)?;
        for pidl in pidls {
            CoTaskMemFree(Some(pidl.cast()));
        }
        // Enum after GetTitle as well as after GetState: selection capture
        // must not depend solely on the slow-state call order.
        let title = root.GetTitle(&items)?;
        println!("title={}", String::from_utf16_lossy(title.as_wide()));
        CoTaskMemFree(Some(title.0.cast()));
        assert_eq!(root.GetCanonicalName()?, class);
        assert_eq!(root.GetFlags()?, ECF_HASSUBCOMMANDS.0 as u32);
        let enumerate = || -> Result<Vec<String>> {
            let commands = root.EnumSubCommands()?;
            let mut targets = Vec::new();
            let mut canonical = Vec::new();
            loop {
                let mut command = [None];
                let status = commands.Next(&mut command, None);
                if status == HRESULT(1) {
                    break;
                }
                status.ok()?;
                let command = command[0].take().expect("one command");
                let title = command.GetTitle(&items)?;
                let title_text = String::from_utf16_lossy(title.as_wide()).to_ascii_lowercase();
                let id = command.GetCanonicalName()?;
                assert_ne!(id, class, "a leaf must not use the root canonical name");
                assert!(
                    !canonical.contains(&id),
                    "leaf canonical names must be distinct"
                );
                if title_text == "mais" {
                    assert_eq!(
                        id, more_id,
                        "More has a stable name distinct from every format"
                    );
                } else {
                    assert_ne!(id, more_id, "More is not a format token");
                }
                assert_eq!(command.GetFlags()?, 0, "only the root has subcommands");
                assert!(
                    command.EnumSubCommands().is_err(),
                    "leaf commands cannot enumerate the root again"
                );
                canonical.push(id);
                targets.push(title_text);
                CoTaskMemFree(Some(title.0.cast()));
            }
            Ok(targets)
        };
        let before_state = enumerate()?;
        let state = root.GetState(&items, true)?;
        let mut actual = enumerate()?;
        let cli = PathBuf::from(&args[0]).parent().unwrap().join("convt.exe");
        let expected_targets = || {
            let mut by_extension = HashMap::new();
            let menus: Vec<_> = paths
                .iter()
                .map(|path| {
                    let Some(extension) = path.extension().map(|ext| ext.to_ascii_lowercase())
                    else {
                        return MenuProbe::default();
                    };
                    by_extension
                        .entry(extension)
                        .or_insert_with(|| {
                            let output = std::process::Command::new(&cli)
                                .arg("targets")
                                .arg(path)
                                .arg("--menu")
                                .arg("--json")
                                .output()
                                .unwrap();
                            if output.status.success() {
                                serde_json::from_slice::<MenuProbe>(&output.stdout)
                                    .unwrap_or_default()
                            } else {
                                MenuProbe::default()
                            }
                        })
                        .clone()
                })
                .collect();
            if menus.is_empty() || menus.iter().any(|menu| !menu.more) {
                return vec![];
            }
            let lists: Vec<_> = menus.into_iter().map(|menu| menu.targets).collect();
            let mut expected = convt_shell::common_targets(&lists);
            expected.push("mais".into());
            expected
        };
        let expected = expected_targets();
        let enumerator = root.EnumSubCommands()?;
        let status = (Interface::vtable(&enumerator).Skip)(
            Interface::as_raw(&enumerator),
            actual.len() as u32 + 1,
        );
        assert_eq!(status, HRESULT(1), "Skip past the end returns S_FALSE");
        enumerator.Reset()?;
        let clone = enumerator.Clone()?;
        let mut fetched = 0;
        let mut batch = vec![None; actual.len() + 1];
        assert_eq!(clone.Next(&mut batch, Some(&mut fetched)), HRESULT(1));
        assert_eq!(fetched as usize, actual.len());
        drop(batch);
        drop(clone);
        drop(enumerator);
        assert_eq!(actual, expected);
        assert_eq!(before_state, expected);
        if !actual.is_empty() {
            assert_eq!(actual.last().unwrap(), "mais", "More must be the last item");
        }
        assert_eq!(
            state,
            if expected.is_empty() {
                ECS_HIDDEN.0 as u32
            } else {
                ECS_ENABLED.0 as u32
            }
        );
        println!("PASS DLL targets: {:?} -> {:?}", paths, actual);
        if let Some(replacement) = refresh {
            // The caller must give this proof its own config directory; never
            // fall back to the user's normal settings location.
            let config = PathBuf::from(
                std::env::var_os("CONVT_CONFIG_DIR")
                    .expect("--refresh-settings requires an isolated CONVT_CONFIG_DIR"),
            );
            std::fs::copy(replacement, config.join("settings.toml")).unwrap();
            let refreshed = enumerate()?;
            assert_eq!(refreshed, expected_targets());
            let state = root.GetState(&items, true)?;
            assert_eq!(
                state,
                if refreshed.is_empty() {
                    ECS_HIDDEN.0 as u32
                } else {
                    ECS_ENABLED.0 as u32
                }
            );
            println!(
                "PASS same COM instance refreshed targets immediately: {:?}",
                refreshed
            );
            actual = refreshed;
        }
        // Optional capture fixture lives beside a COPY of the real DLL/CLI.
        // It proves Invoke passes UTF-16 paths safely without any GUI input.
        if capture.is_some() && !actual.is_empty() {
            let commands = root.EnumSubCommands()?;
            let index = if capture_more {
                assert_eq!(actual.last().unwrap(), "mais");
                actual.len() - 1
            } else {
                actual
                    .iter()
                    .position(|target| target != "mais")
                    .expect("format capture requires a quick format; use --capture-more for More")
            };
            commands.Skip(index as u32)?;
            let mut command = [None];
            commands.Next(&mut command, None).ok()?;
            let command = command[0].take().unwrap();
            if capture_more {
                assert_eq!(
                    command.GetCanonicalName()?,
                    more_id,
                    "capture must invoke More itself"
                );
            } else {
                assert_ne!(
                    command.GetCanonicalName()?,
                    more_id,
                    "format capture must not invoke More"
                );
            }
            command.Invoke(&items, None)?;
            println!(
                "PASS Invoke {} dispatched {} paths",
                if capture_more { "More" } else { "format" },
                paths.len()
            );
        }
        // Deliberately do not FreeLibrary: COM references still own DLL code.
        drop(OsString::from_wide(&[]));
        // Registered activation can unload the server at CoUninitialize.
        // Release its objects while their COM apartment is still alive.
        drop(root);
        drop(items);
        CoUninitialize();
    }
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Run explorer-probe on Windows");
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS};
    use windows::core::{Error, HRESULT};

    #[test]
    fn capture_checks_default_and_the_same_raw_utf16_key_as_the_app_and_shell() {
        assert_eq!(
            capture_mutex_names(OsStr::new(r"C:\convt-runtime-ação")),
            [
                "Local\\convt-instance".to_string(),
                "Local\\convt-instance-bafbcab710422a4c".to_string(),
            ]
        );
        assert_ne!(
            capture_mutex_names(OsStr::new(r"C:\runtime\inner\..\inbox"))[1],
            capture_mutex_names(OsStr::new(r"C:\runtime\inbox"))[1],
            "the guard must match the raw environment value, not a canonical path"
        );
    }

    #[test]
    fn a_running_app_in_the_override_namespace_is_rejected_before_invoke() {
        let runtime = OsStr::new(r"C:\convt-runtime-ação");
        let scoped = capture_mutex_names(runtime)[1].clone();
        let mut checked = Vec::new();
        let error = require_idle_capture_runtime(runtime, |name| {
            checked.push(name.to_string());
            Ok(name == scoped)
        })
        .unwrap_err();
        assert_eq!(checked, capture_mutex_names(runtime));
        assert_eq!(error.code(), HRESULT::from_win32(ERROR_ALREADY_EXISTS.0));
    }

    #[test]
    fn mutex_query_errors_fail_closed_and_only_two_absent_names_pass() {
        let runtime = OsStr::new(r"C:\isolated-inbox");
        for fail_at in [0, 1] {
            let mut checked = 0;
            let error = require_idle_capture_runtime(runtime, |_| {
                let current = checked;
                checked += 1;
                if current == fail_at {
                    Err(Error::from_hresult(HRESULT::from_win32(
                        ERROR_ACCESS_DENIED.0,
                    )))
                } else {
                    Ok(false)
                }
            })
            .unwrap_err();
            assert_eq!(error.code(), HRESULT::from_win32(ERROR_ACCESS_DENIED.0));
            assert_eq!(checked, fail_at + 1);
        }
        let mut checked = 0;
        require_idle_capture_runtime(runtime, |_| {
            checked += 1;
            Ok(false)
        })
        .unwrap();
        assert_eq!(checked, 2);
    }
}
