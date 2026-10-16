//! `convt menu` and `convt targets --menu` as the binary runs them. Every
//! test gets its own config folder, so the user's `menu.toml` is never read.
//! Only image formats are used: the `image` engine is always there.

use std::path::PathBuf;
use std::process::Output;

struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("menu.toml")
    }

    fn saved(&self) -> String {
        std::fs::read_to_string(self.file()).unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_convt"))
            .args(args)
            .env("CONVT_CONFIG_DIR", self.dir.path())
            .env("CONVT_DATA_DIR", self.dir.path().join("data"))
            .env("CONVT_LICENSE_STORE", "file")
            .env("CONVT_LICENSE_ENFORCE", "0")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn menu_for(&self, name: &str) -> String {
        self.ok(&["targets", name, "--menu"])
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn line<'a>(show: &'a str, kind: &str) -> &'a str {
    show.lines()
        .find(|l| l.split_whitespace().next() == Some(kind))
        .unwrap_or_else(|| panic!("no {kind} line in {show}"))
}

#[test]
fn set_keeps_the_order_and_menus_follow_it() {
    let f = Fixture::new();
    let defaults = f.menu_for("x.bmp");
    assert!(line(&f.ok(&["menu"]), "image").ends_with("(default)"));

    f.ok(&["menu", "set", "image", "webp", "tiff", "webp"]);
    assert_eq!(f.saved().trim(), r#"image = ["webp", "tiff"]"#);
    let show = f.ok(&["menu", "show"]);
    assert_eq!(
        line(&show, "image").split_whitespace().collect::<Vec<_>>(),
        ["image", "webp", "tiff"]
    );
    assert!(line(&show, "vector").ends_with("(default)"));
    assert_eq!(f.menu_for("x.bmp"), "webp tiff");
    // A file never offers its own format.
    assert_eq!(f.menu_for("x.webp"), "tiff");
    assert_ne!(f.menu_for("x.bmp"), defaults);
}

#[test]
fn rejected_changes_leave_the_file_alone() {
    let f = Fixture::new();
    f.ok(&["menu", "set", "image", "png"]);
    let before = f.saved();
    for args in [
        &["menu", "set", "nope", "png"][..],
        &["menu", "set", "image", "zzz"],
        &["menu", "set", "image", "mp3"],
        &["menu", "reset", "nope"],
    ] {
        let out = f.run(args);
        assert!(!out.status.success(), "{args:?} should fail");
        assert!(!stderr(&out).is_empty(), "{args:?} says why");
        assert_eq!(f.saved(), before, "{args:?}");
    }
    assert!(stderr(&f.run(&["menu", "set", "nope", "png"])).contains("image"));
}

#[test]
fn reset_one_kind_keeps_the_others_and_reset_all_repairs_the_file() {
    let f = Fixture::new();
    f.ok(&["menu", "set", "image", "tiff"]);
    f.ok(&["menu", "set", "vector", "webp"]);
    f.ok(&["menu", "reset", "image"]);
    assert_eq!(f.saved().trim(), r#"vector = ["webp"]"#);

    std::fs::write(f.file(), "image = 3").unwrap();
    let out = f.run(&["menu", "show"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("menu.toml"), "{}", stderr(&out));
    // Menus fall back to the defaults instead of failing.
    assert!(!f.menu_for("x.bmp").is_empty());

    f.ok(&["menu", "reset"]);
    assert!(line(&f.ok(&["menu"]), "image").ends_with("(default)"));
}

impl Fixture {
    fn menu_json(&self, name: &str) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["targets", name, "--menu", "--json"])).unwrap()
    }
}

#[test]
fn json_menu_is_the_picked_list_with_more_options() {
    // The Windows Explorer handler reads this form: the same targets as the
    // plain menu, plus More options for Quick convert.
    let f = Fixture::new();
    let plain: Vec<String> = f.menu_for("x.bmp").split(' ').map(str::to_owned).collect();
    assert_eq!(
        f.menu_json("x.bmp"),
        serde_json::json!({"targets": plain, "more": true})
    );
    f.ok(&["menu", "set", "image", "webp", "tiff"]);
    assert_eq!(
        f.menu_json("x.bmp"),
        serde_json::json!({"targets": ["webp", "tiff"], "more": true})
    );
}

#[test]
fn json_needs_menu_and_unknown_files_fail_without_stdout() {
    let f = Fixture::new();
    for (args, code) in [
        (&["targets", "x.bmp", "--json"][..], 2),
        (&["targets", "x.unknown", "--menu", "--json"], 1),
    ] {
        let out = f.run(args);
        assert_eq!(out.status.code(), Some(code), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        assert!(!stderr(&out).is_empty(), "{args:?}");
    }
}

#[test]
fn json_keeps_more_options_without_a_media_engine() {
    // Explorer still offers More options for a video when FFmpeg is missing.
    let f = Fixture::new();
    let binary = f
        .dir
        .path()
        .join(format!("convt{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_convt"), &binary).unwrap();
    let out = std::process::Command::new(binary)
        .args(["targets", "x.mp4", "--menu", "--json"])
        .env("PATH", "")
        .env_remove("CONVT_FFMPEG")
        .env("CONVT_CONFIG_DIR", f.dir.path())
        .env("CONVT_DATA_DIR", f.dir.path().join("data"))
        .env("CONVT_LICENSE_STORE", "file")
        .env("CONVT_LICENSE_ENFORCE", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
        serde_json::json!({"targets": [], "more": true})
    );
}
