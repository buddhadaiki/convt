#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output};

fn cli(root: &Path, args: &[&std::ffi::OsStr]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_convt"));
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
        .env("CONVT_CONFIG_DIR", root.join("config"))
        .env("CONVT_DATA_DIR", root.join("data"))
        .env("CONVT_LICENSE_STORE", "file")
        .env_remove("CONVT_LICENSE_ENFORCE")
        .args(args)
        .output()
        .unwrap()
}

#[cfg(windows)]
fn settings(root: &Path, contents: &str) {
    std::fs::create_dir_all(root.join("config")).unwrap();
    std::fs::write(root.join("config/settings.toml"), contents).unwrap();
}

fn successful(out: &Output) {
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn targets_file(root: &Path, file: &Path, menu: bool) -> Vec<String> {
    let mut args = vec!["targets".as_ref(), file.as_os_str()];
    if menu {
        args.push("--menu".as_ref());
    }
    let out = cli(root, &args);
    successful(&out);
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(windows)]
fn targets(root: &Path, menu: bool) -> Vec<String> {
    targets_file(root, Path::new("sample.png"), menu)
}

fn menu_json(root: &Path, file: &Path) -> serde_json::Value {
    let out = cli(
        root,
        &[
            "targets".as_ref(),
            file.as_os_str(),
            "--menu".as_ref(),
            "--json".as_ref(),
        ],
    );
    successful(&out);
    serde_json::from_slice(&out.stdout).unwrap()
}

#[cfg(windows)]
#[test]
fn windows_menu_disabled_and_empty_keep_conversions_available() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let all = targets(root, false);
    assert!(all.iter().any(|id| id == "jpeg"));
    assert!(!targets(root, true).is_empty());
    settings(root, "[windows_menu]\nenabled = false\n");
    assert!(targets(root, true).is_empty());
    assert_eq!(
        menu_json(root, Path::new("sample.png")),
        serde_json::json!({"targets": [], "more": false})
    );
    assert_eq!(targets(root, false), all);

    let input = root.join("sample.png");
    image::RgbImage::from_fn(4, 3, |x, y| image::Rgb([x as u8 * 30, y as u8 * 40, 100]))
        .save(&input)
        .unwrap();
    let output = root.join("output");
    std::fs::create_dir(&output).unwrap();
    let out = cli(
        root,
        &[
            input.as_os_str(),
            "--to".as_ref(),
            "jpeg".as_ref(),
            "--out-dir".as_ref(),
            output.as_os_str(),
        ],
    );
    successful(&out);
    let jpeg = output.join("sample.jpg");
    assert_eq!(image::image_dimensions(&jpeg).unwrap(), (4, 3));
    assert!(
        std::fs::read(&jpeg)
            .unwrap()
            .starts_with(&[0xff, 0xd8, 0xff])
    );

    for config in [
        "[windows_menu]\ntargets = []\n",
        "[windows_menu]\ntargets = ['unknown', 'pdf', 'png']\n",
    ] {
        settings(root, config);
        assert!(targets(root, true).is_empty());
        assert_eq!(
            menu_json(root, Path::new("sample.png")),
            serde_json::json!({"targets": [], "more": true})
        );
        assert_eq!(targets(root, false), all);
    }
}

#[cfg(windows)]
#[test]
fn windows_menu_custom_targets_follow_reachability_and_configured_order() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    assert!(targets(root, false).iter().any(|id| id == "tga"));
    assert!(!targets(root, true).iter().any(|id| id == "tga"));
    settings(
        root,
        "[windows_menu]\ntargets = ['tga', 'unknown', 'webp', 'tga', 'mp4', 'jpeg', 'png']\n",
    );
    assert_eq!(targets(root, true), ["tga", "webp", "jpeg"]);
}

#[cfg(windows)]
#[test]
fn windows_menu_missing_section_preserves_defaults_and_ignores_other_settings() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let original = targets(root, true);
    assert!(!original.is_empty());
    settings(
        root,
        "notifications = 'future schema value'\n[other]\nvalue = 42\n",
    );
    assert_eq!(targets(root, true), original);
    settings(root, "[windows_menu]\n");
    assert_eq!(targets(root, true), original);
    settings(
        root,
        "concurrency = 'future schema value'\n[windows_menu]\ntargets = ['webp']\n",
    );
    assert_eq!(targets(root, true), ["webp"]);
}

#[cfg(windows)]
#[test]
fn windows_menu_malformed_settings_fail_explicitly_without_limiting_full_targets() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let all = targets(root, false);
    for malformed in ["[windows_menu", "[windows_menu]\nenabled = 'no'\n"] {
        settings(root, malformed);
        for json in [false, true] {
            let mut args = vec!["targets".as_ref(), "sample.png".as_ref(), "--menu".as_ref()];
            if json {
                args.push("--json".as_ref());
            }
            let out = cli(root, &args);
            assert_eq!(out.status.code(), Some(1));
            assert!(out.stdout.is_empty());
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains("settings.toml"), "{stderr}");
            assert!(stderr.contains("parse"), "{stderr}");
        }
        assert_eq!(targets(root, false), all);
    }
}

#[test]
fn targets_json_requires_menu_and_unknown_extensions_fail_without_stdout() {
    let root = tempfile::tempdir().unwrap();
    for (file, menu, code) in [("sample.png", false, 2), ("sample.unknown", true, 1)] {
        let mut args = vec!["targets".as_ref(), file.as_ref(), "--json".as_ref()];
        if menu {
            args.push("--menu".as_ref());
        }
        let out = cli(root.path(), &args);
        assert_eq!(out.status.code(), Some(code));
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
}

#[test]
fn menu_json_keeps_more_for_a_known_extension_without_a_media_engine() {
    let root = tempfile::tempdir().unwrap();
    let binary = root
        .path()
        .join(format!("convt{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_convt"), &binary).unwrap();
    let mut command = Command::new(binary);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let out = command
        .env("PATH", "")
        .env_remove("CONVT_FFMPEG")
        .env("CONVT_CONFIG_DIR", root.path().join("config"))
        .env("CONVT_DATA_DIR", root.path().join("data"))
        .env("CONVT_LICENSE_STORE", "file")
        .env_remove("CONVT_LICENSE_ENFORCE")
        .args(["targets", "sample.mp4", "--menu", "--json"])
        .output()
        .unwrap();
    successful(&out);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
        serde_json::json!({"targets": [], "more": true})
    );
}

#[cfg(not(windows))]
#[test]
fn non_windows_json_preserves_registry_menu_recommendations() {
    let root = tempfile::tempdir().unwrap();
    let file = Path::new("sample.png");
    let expected = targets_file(root.path(), file, true);
    assert_eq!(
        menu_json(root.path(), file),
        serde_json::json!({"targets": expected, "more": true})
    );
}

#[cfg(windows)]
#[test]
fn windows_video_quick_menu_keeps_gif_and_full_audio_conversion() {
    let Some(ffmpeg) = convt_engines::ffmpeg::ffmpeg_path() else {
        eprintln!("SKIP video menu: FFmpeg is unavailable");
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    let input = root.join("sample.mp4");
    let out = Command::new(&ffmpeg)
        .creation_flags(0x08000000)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=32x24:rate=10",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=44100",
            "-t",
            "0.2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
        ])
        .arg(&input)
        .output()
        .unwrap();
    successful(&out);
    let full = targets_file(root, &input, false);
    assert!(full.iter().any(|id| id == "mp3"));
    assert_eq!(
        targets_file(root, &input, true),
        ["mov", "webm", "mkv", "gif"]
    );
    assert_eq!(
        menu_json(root, &input),
        serde_json::json!({"targets": ["mov", "webm", "mkv", "gif"], "more": true})
    );
    settings(
        root,
        "[windows_menu]\ntargets = ['mp3', 'gif', 'mkv', 'mov', 'gif', 'wav']\n",
    );
    assert_eq!(targets_file(root, &input, true), ["gif", "mkv", "mov"]);
    assert_eq!(targets_file(root, &input, false), full);
    settings(
        root,
        "[windows_menu]\ntargets = ['mp4', 'png', 'wav', 'gif', 'flac', 'wav']\n",
    );
    assert_eq!(
        targets_file(root, Path::new("sample.mp3"), true),
        ["wav", "flac"]
    );
    let output = root.join("output");
    std::fs::create_dir(&output).unwrap();
    successful(&cli(
        root,
        &[
            input.as_os_str(),
            "--to".as_ref(),
            "mp3".as_ref(),
            "--out-dir".as_ref(),
            output.as_os_str(),
        ],
    ));
    let ffprobe = std::env::var_os("CONVT_FFPROBE")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let bundled = ffmpeg.with_file_name("ffprobe.exe");
            bundled.is_file().then_some(bundled)
        })
        .or_else(|| which::which("ffprobe").ok());
    let Some(ffprobe) = ffprobe else {
        eprintln!("SKIP independent MP3 output validation: ffprobe is unavailable");
        return;
    };
    let out = Command::new(ffprobe)
        .creation_flags(0x08000000)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name,codec_type",
            "-of",
            "json",
        ])
        .arg(output.join("sample.mp3"))
        .output()
        .unwrap();
    successful(&out);
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let streams = info["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0]["codec_type"], "audio");
    assert_eq!(streams[0]["codec_name"], "mp3");
}
