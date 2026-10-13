# Windows Explorer menu

The per-user MSI builds and installs `convt_shell.dll` next to `convt.exe` and
`convt-app.exe`. It registers the COM class
`{710FB9A8-C47E-4B39-9CFA-E273AB1B78F8}` and **Convert with convt** under the
current user's `Software\Classes` keys. The verb applies to files, not folders,
and changes no file association or double-click default. Uninstall removes the
MSI's authored registry values and payload; settings and history remain.

On Windows 11 build 22000 or later, the verification MSI also registers the
external-location identity `Convt.Desktop` / `CN=Convt Local` / `ConvtApp` when
Developer Mode is already enabled. Its manifest declares the same COM class
in an STA surrogate and exposes it to the main context menu. Registration is
per-user and reuses the installed binaries; no certificate, policy or Developer
Mode setting is changed. The classic file verb is backed up and removed only
after the modern package, its external location and an actual out-of-process
`IExplorerCommand` activation are confirmed. The activation check runs in a
disposable child process with a 20-second timeout. A failed check rolls back
registration and preserves the classic verb. If those
prerequisites are absent, **Show more options** keeps the classic command.
See [Microsoft's packaged context-menu contract](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/integrate-packaged-app-with-file-explorer)
and [the Windows packaging guide](../../packaging/windows/README.md).

The MSI pairs registration and removal with transaction-specific Appx rollback.
Journals live under `%LOCALAPPDATA%\Convt\installer-transactions`, outside the
removable payload. Uninstall preserves settings/history. Upgrade snapshots the
identity at the previous MSI installation path, validates its publisher, version
and external root, and transfers it after installing the new payload. A guarded
backup of package entrypoints, manifest and logos supports compensation before
MSI restores the rest of the old runtime. Unrelated identities and divergent
files are rejected before modification. Public delivery
still needs a signed identity package; this MSI's loose registration is for a
machine with Developer Mode already enabled.

The successful finish page defaults to enabling the context menu and leaving
the desktop shortcut unchecked. **Abrir convt** and **Concluir** apply those
choices before opening the app or closing the installer. Saved installer choices
are reused on maintenance; they do not rewrite the app's menu format preferences.

## Choose the formats

In the app's Settings, open **Windows menu**. Enable or disable the menu, use
the recommended formats, or choose a custom set. Quick targets must be reachable
from every selected input and belong to the input's family: images/vectors,
audio, video, or documents/PDF/spreadsheets/presentations. Video also offers GIF.
For MP4, the default list is MOV, WebM, MKV and GIF when the installed engines
support them. Extracting MP3 is available through **Mais**.

**Mais** is the last submenu item. It opens Quick convert with the selected
files, no chosen target and no automatic conversion. That window retains all
compatible conversions, including those outside the quick menu or its custom
format list. An empty custom set or an empty intersection leaves **Mais** alone.
A disabled menu, an unknown extension, a folder selection or a failed CLI probe
hide the command. Recognized files without an available engine can still open
the app through **Mais**, including its document-pack prompt. The next menu
construction reads the saved preferences without a target cache.

The handler asks the CLI beside its own DLL for
`convt.exe targets <file> --menu --json`. The response contains `targets` (format
IDs) and `more` (whether Mais is available); the text form of `--menu` still
prints format IDs only. It never searches `PATH` or loads conversion
engines into Explorer. Each query deduplicates files by extension, preserves
the first file's menu order and intersects the targets across the selection.
Probes run without a console, have a two-second deadline and bounded output.
The selection is limited to 256 files. Choosing a target writes a request
with the file paths, the format and `show_progress=true` to the app's
single-instance inbox; the app shows Activity while converting. Mais uses
`to=null`, `preset=null`, `show_progress=false` and the same inbox, so the
existing request route opens Quick convert and waits for Convert. Paths are
JSON data, not a shell command line.

When the app is closed, the handler launches `convt-app --from-explorer`, which
starts only the inbox listener. It does not add the empty launch request that
would open Activity behind Quick convert. A racing second Explorer launch also
does not enqueue an empty request. Opening convt normally still shows Activity;
an Activity window already opened by the user is preserved.

An explicit `CONVT_RUNTIME_DIR` isolates both the inbox and the app/shell mutex
using the same UTF-16 key. Without an override, the installed namespace remains
`Local\\convt-instance`. Use the exact same absolute override value in every
process of a fixture, along with separate config/data directories.

## Build and probe the handler

In a Windows x64 checkout with Rust and the C++ build tools:

```powershell
cargo build --target x86_64-pc-windows-msvc -p convt-cli -p convt-shell
cargo build --target x86_64-pc-windows-msvc -p convt-shell --example explorer-probe
.\target\x86_64-pc-windows-msvc\debug\examples\explorer-probe.exe `
  .\target\x86_64-pc-windows-msvc\debug\convt_shell.dll C:\test\sample.png
```

The probe loads `DllGetClassObject`, constructs `IExplorerCommand`, enumerates
before and after `GetState`, tests the enumerator and compares the submenu with
the CLI beside the DLL, including Mais last and its distinct canonical GUID.
Give it several input types to check target intersection.
It does not register the DLL or invoke a conversion. After an authorized MSI
installation, `--registered` tests COM activation from its existing registration.
Use `--modern` instead to require `CLSCTX_LOCAL_SERVER`, proving the packaged
surrogate rather than the classic in-process class. It does not invoke the app.

Inspect the installed identity without changing it:

```powershell
& "$env:LOCALAPPDATA\Programs\convt\identity\modern-menu.ps1" -Action Status
```

To prove that the same COM object observes new preferences immediately, set
`CONVT_CONFIG_DIR` to a fresh test directory with a `settings.toml`, then pass
`--refresh-settings <replacement.toml>` before the DLL path. The probe copies
that replacement only to the explicitly supplied config directory and checks
the refreshed targets and visibility. Keep test settings separate from the
user's own configuration. Do not set `CONVT_SHELL_CAPTURE` unless an isolated
capture fixture has replaced a copied `convt-app.exe`; that opt-in invokes a
target. `--capture-more` invokes only Mais instead. Capture requires an explicit
private runtime directory, a small adjacent capture stub and no running real
app; registered/modern activation refuses capture. Never substitute the stub
in the user's actual installation.

The probe and compiled-MSI registry table tests are separate from a real
Explorer right-click, installation, rollback and uninstallation checks. Those
native checks require permission to operate the user's desktop and registry.
