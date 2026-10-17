"""Native installer process supervision, without MSI, Appx or real integration.

Requires existing x64 MSVC/Windows SDK tools; discovers vcvars64 if needed.
Every process is an owned fixture executable, and temporary files are removed.
"""

import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

HERE = Path(__file__).resolve().parent


def compiler_environment():
    env = os.environ.copy()
    if shutil.which("cl.exe") and env.get("INCLUDE") and env.get("LIB"):
        return env
    vswhere = Path(env.get("ProgramFiles(x86)", "C:/Program Files (x86)")) / "Microsoft Visual Studio/Installer/vswhere.exe"
    if not vswhere.is_file():
        raise unittest.SkipTest("requires existing x64 MSVC and Windows SDK")
    found = subprocess.run([str(vswhere), "-latest", "-products", "*", "-requires",
                            "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-format", "json"],
                           capture_output=True, text=True, check=True, timeout=20)
    installations = json.loads(found.stdout)
    if not installations:
        raise unittest.SkipTest("requires existing x64 MSVC and Windows SDK")
    vcvars = Path(installations[0]["installationPath"]) / "VC/Auxiliary/Build/vcvars64.bat"
    imported = subprocess.run([env.get("COMSPEC", "cmd.exe"), "/d", "/s", "/c",
                               f'"{vcvars}" >nul && set'], capture_output=True, text=True,
                              check=True, timeout=30)
    for line in imported.stdout.splitlines():
        key, separator, value = line.partition("=")
        if separator and key.upper() in {"PATH", "INCLUDE", "LIB", "LIBPATH"}:
            env[key.upper()] = value
    return env


@unittest.skipUnless(os.name == "nt", "requires Windows process and Job Object APIs")
class InstallerProcessTree(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.work = tempfile.TemporaryDirectory(prefix="convt-installer-process-")
        cls.addClassCleanup(cls.work.cleanup)
        cls.executable = Path(cls.work.name) / "installer-process-fixture.exe"
        env = compiler_environment()
        compiler = shutil.which("cl.exe", path=env.get("PATH"))
        if not compiler:
            raise unittest.SkipTest("requires existing x64 MSVC and Windows SDK")
        build = subprocess.run([compiler, "/nologo", "/MT", "/std:c++17", "/EHsc", "/W4", "/WX", "/utf-8",
                                "/DUNICODE", "/D_UNICODE", str(HERE / "installer-process-fixture.cpp"),
                                f"/Fe{cls.executable}", f"/Fo{Path(cls.work.name) / 'fixture.obj'}",
                                "/link", "/MACHINE:X64", "advapi32.lib"],
                               env=env, cwd=cls.work.name, capture_output=True, text=True, timeout=60)
        if build.returncode:
            raise RuntimeError("Native process fixture build failed:\n" + build.stdout + build.stderr)
        cls.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        cls.kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        cls.kernel.OpenProcess.restype = wintypes.HANDLE
        cls.kernel.QueryFullProcessImageNameW.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.LPWSTR,
                                                         ctypes.POINTER(wintypes.DWORD)]
        cls.kernel.QueryFullProcessImageNameW.restype = wintypes.BOOL
        cls.kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
        cls.kernel.WaitForSingleObject.restype = wintypes.DWORD
        cls.kernel.TerminateProcess.argtypes = [wintypes.HANDLE, wintypes.UINT]
        cls.kernel.TerminateProcess.restype = wintypes.BOOL
        cls.kernel.CloseHandle.argtypes = [wintypes.HANDLE]
        cls.kernel.CloseHandle.restype = wintypes.BOOL

    def owned_process(self, pid):
        handle = self.kernel.OpenProcess(0x00100000 | 0x1000 | 0x0001, False, pid)
        if not handle:
            error = ctypes.get_last_error()
            if error == 87:  # ERROR_INVALID_PARAMETER: this owned process already exited.
                return None
            raise ctypes.WinError(error)
        path = ctypes.create_unicode_buffer(32768)
        length = wintypes.DWORD(len(path))
        if not self.kernel.QueryFullProcessImageNameW(handle, 0, path, ctypes.byref(length)):
            error = ctypes.get_last_error()
            exited = self.kernel.WaitForSingleObject(handle, 0) == 0
            self.kernel.CloseHandle(handle)
            if exited:
                return None
            raise ctypes.WinError(error)
        if Path(path.value).resolve() != self.executable.resolve():
            self.kernel.CloseHandle(handle)
            raise AssertionError("Refusing to operate on a process outside the owned fixture")
        return handle

    def scenario(self, name):
        with tempfile.TemporaryDirectory(prefix="scenario-", dir=self.work.name) as directory:
            root = Path(directory)
            controller = subprocess.Popen([str(self.executable), "controller", name, directory],
                                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                          creationflags=subprocess.CREATE_NO_WINDOW)
            handles = {}

            def collect():
                for role in ["script", "leaf"]:
                    pid_file = root / f"{role}.pid"
                    if role not in handles and pid_file.is_file() and pid_file.stat().st_size:
                        handles[role] = self.owned_process(int(pid_file.read_text()))

            try:
                deadline = time.monotonic() + 7
                while not (root / "leaf.pid").is_file():
                    if controller.poll() is not None or time.monotonic() >= deadline:
                        break
                    collect()
                    time.sleep(0.01)
                collect()
                output, error = controller.communicate(timeout=7)
                self.assertEqual(controller.returncode, 0, output + error)
                self.assertEqual(set(handles), {"script", "leaf"}, output + error)
                for role, handle in handles.items():
                    if handle:
                        self.assertEqual(self.kernel.WaitForSingleObject(handle, 2000), 0,
                                         f"{name} left the owned {role} running after supervision returned")
            finally:
                if controller.poll() is None:
                    controller.kill()
                    controller.communicate(timeout=5)
                try:
                    collect()
                finally:
                    for handle in handles.values():
                        if handle:
                            if self.kernel.WaitForSingleObject(handle, 0) == 258:
                                self.kernel.TerminateProcess(handle, 1)
                                self.kernel.WaitForSingleObject(handle, 5000)
                            self.kernel.CloseHandle(handle)

    def test_interruption_reaps_the_script_and_probe_before_returning(self):
        self.scenario("interrupted")

    def test_controller_exit_does_not_orphan_a_probe_holding_a_pipe(self):
        self.scenario("controller-exit")

    def test_completed_script_cannot_leave_a_probe_behind(self):
        self.scenario("completed")

    def test_probe_that_exited_before_the_script_does_not_fail_cleanup(self):
        self.scenario("short-probe")


if __name__ == "__main__":
    unittest.main()
