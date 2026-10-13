use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        panic!("convt-app Windows resources require the MSVC target");
    }

    for variable in [
        "RC",
        "PATH",
        "WindowsSdkVerBinPath",
        "WindowsSdkBinPath",
        "WindowsSdkDir",
        "WindowsSDKVersion",
        "ProgramFiles(x86)",
        "ProgramFiles",
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
    }

    let crate_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let repo = crate_dir.parent().unwrap().parent().unwrap();
    let template = crate_dir.join("resources/convt-app.rc");
    let manifest = crate_dir.join("resources/convt-app.manifest");
    let icon = repo.join("packaging/windows/convt-v2.ico");
    for path in [&template, &manifest, &icon] {
        println!("cargo:rerun-if-changed={}", path.display());
        assert!(
            path.is_file(),
            "Missing Windows resource: {}",
            path.display()
        );
    }

    let output_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let generated = output_dir.join("convt-app.rc");
    let resource = output_dir.join("convt-app.res");
    let source = fs::read_to_string(&template).expect("Cannot read Windows resource template");
    let source = source.replace("@CONVT_APP_ICON@", &resource_path(&icon));
    // UTF-16 keeps SDK rc.exe independent of the machine's ANSI code page.
    let bytes: Vec<u8> = std::iter::once(0xfeff_u16)
        .chain(source.encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    fs::write(&generated, bytes).expect("Cannot write Windows resource source");

    let rc = find_rc().unwrap_or_else(|error| panic!("{error}"));
    let status = Command::new(&rc)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource)
        .arg(&generated)
        .status()
        .unwrap_or_else(|error| panic!("Cannot run {}: {error}", rc.display()));
    assert!(
        status.success(),
        "Windows resource compiler failed: {status}"
    );
    // GPUI supplies RT_MANIFEST 1. Packaging merges our identity fragment into
    // that resource after linking, preserving GPUI's DPI and compatibility flags.
    println!("cargo:rustc-link-arg-bin=convt-app={}", resource.display());
}

fn resource_path(path: &Path) -> String {
    assert!(path.is_absolute(), "Resource paths must be absolute");
    let path = path.to_str().expect("Windows resource path is not Unicode");
    assert!(
        !path.contains(['"', '\r', '\n']),
        "Windows resource path contains unsupported characters"
    );
    path.replace('\\', "\\\\")
}

fn find_rc() -> Result<PathBuf, String> {
    if let Some(override_path) = env::var_os("RC") {
        let path = PathBuf::from(override_path);
        if path.is_file() {
            return Ok(path);
        }
        if let Some(path) = on_path(&path) {
            return Ok(path);
        }
        return Err(format!(
            "RC does not identify a resource compiler: {}",
            path.display()
        ));
    }
    let host = env::var("HOST").unwrap_or_default();
    let architecture = if host.starts_with("aarch64") {
        "arm64"
    } else if host.starts_with("i686") {
        "x86"
    } else {
        "x64"
    };
    if let Some(path) = env::var_os("WindowsSdkVerBinPath") {
        let rc = PathBuf::from(path).join(architecture).join("rc.exe");
        if rc.is_file() {
            return Ok(rc);
        }
    }
    let version = env::var("WindowsSDKVersion")
        .unwrap_or_default()
        .trim_end_matches(['\\', '/'])
        .to_owned();
    let mut sdk_bins = Vec::new();
    if let Some(path) = env::var_os("WindowsSdkBinPath") {
        sdk_bins.push(PathBuf::from(path));
    }
    if let Some(path) = env::var_os("WindowsSdkDir") {
        sdk_bins.push(PathBuf::from(path).join("bin"));
    }
    if !version.is_empty() {
        for bin in &sdk_bins {
            let rc = bin.join(&version).join(architecture).join("rc.exe");
            if rc.is_file() {
                return Ok(rc);
            }
        }
    }
    if let Some(rc) = on_path(Path::new("rc.exe")) {
        return Ok(rc);
    }
    for variable in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Some(path) = env::var_os(variable) {
            sdk_bins.push(PathBuf::from(path).join("Windows Kits/10/bin"));
        }
    }
    for bin in sdk_bins {
        if let Some(rc) = latest_sdk_rc(&bin, architecture) {
            return Ok(rc);
        }
    }
    Err("Windows SDK rc.exe not found; use the MSVC/SDK environment or set RC".into())
}

fn on_path(program: &Path) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .map(|directory| directory.join(program))
            .find(|path| path.is_file())
    })
}

fn sdk_version(name: &str) -> Option<[u32; 4]> {
    let values: Vec<u32> = name
        .split('.')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    values.try_into().ok()
}

fn latest_sdk_rc(bin: &Path, architecture: &str) -> Option<PathBuf> {
    let mut versions: Vec<_> = fs::read_dir(bin)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let version = sdk_version(entry.file_name().to_str()?)?;
            Some((version, entry.path()))
        })
        .collect();
    versions.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    versions
        .into_iter()
        .map(|(_, directory)| directory.join(architecture).join("rc.exe"))
        .chain(std::iter::once(bin.join(architecture).join("rc.exe")))
        .find(|path| path.is_file())
}
