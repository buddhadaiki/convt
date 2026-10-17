#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include "installer-process.h"

#include <filesystem>
#include <fstream>
#include <iostream>
#include <string>

namespace {
std::wstring quoted(const std::wstring& text) { return L"\"" + text + L"\""; }

void record_pid(const std::filesystem::path& path) {
    std::ofstream output(path);
    output << GetCurrentProcessId();
    if (!output) throw std::runtime_error("could not record owned fixture PID");
}

int script(const std::wstring& executable, const std::filesystem::path& directory, bool exit_early, bool short_probe) {
    record_pid(directory / "script.pid");
    SECURITY_ATTRIBUTES security{sizeof(security), nullptr, TRUE};
    HANDLE reader = nullptr;
    HANDLE writer = nullptr;
    if (!CreatePipe(&reader, &writer, &security, 0) ||
        !SetHandleInformation(reader, HANDLE_FLAG_INHERIT, 0)) return 10;
    HANDLE input = CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE,
                               &security, OPEN_EXISTING, 0, nullptr);
    if (input == INVALID_HANDLE_VALUE) return 11;
    STARTUPINFOW startup{};
    startup.cb = sizeof(startup);
    startup.dwFlags = STARTF_USESTDHANDLES;
    startup.hStdInput = input;
    startup.hStdOutput = writer;
    startup.hStdError = writer;
    PROCESS_INFORMATION created{};
    std::wstring command = quoted(executable) + (short_probe ? L" leaf-short unused " : L" leaf unused ") +
                           quoted(directory.wstring());
    if (!CreateProcessW(executable.c_str(), command.data(), nullptr, nullptr, TRUE,
                        CREATE_NO_WINDOW, nullptr, nullptr, &startup, &created)) return 12;
    CloseHandle(created.hThread);
    CloseHandle(created.hProcess);
    CloseHandle(input);
    CloseHandle(writer);
    if (exit_early) return 0;
    // Like the installer's Probe subprocess, this descendant keeps a pipe
    // open while its controller waits for EOF. Stopping only the script leaks it.
    char buffer[16];
    DWORD read = 0;
    ReadFile(reader, buffer, sizeof(buffer), &read, nullptr);
    CloseHandle(reader);
    if (short_probe) Sleep(200); // Like a short-lived compiler finishing before PowerShell.
    return 0;
}

struct Interrupted {};
} // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc != 4) return 1;
    const std::wstring mode = argv[1];
    const std::filesystem::path directory = argv[3];
    try {
        if (mode == L"leaf" || mode == L"leaf-short") {
            record_pid(directory / "leaf.pid");
            if (mode == L"leaf") Sleep(60000);
            return 0;
        }
        if (mode == L"script" || mode == L"script-exit" || mode == L"script-short") {
            return script(argv[0], directory, mode == L"script-exit", mode == L"script-short");
        }
        if (mode != L"controller") return 2;
        const std::wstring scenario = argv[2];
        const std::wstring script_mode = scenario == L"completed" ? L"script-exit" :
                                         scenario == L"short-probe" ? L"script-short" : L"script";
        convt::installer::ProcessTree process;
        process.start(argv[0], quoted(argv[0]) + L" " + script_mode + L" unused " +
                      quoted(directory.wstring()), CREATE_NO_WINDOW, nullptr, directory.c_str());
        const ULONGLONG started = GetTickCount64();
        while (!std::filesystem::exists(directory / "leaf.pid") ||
               std::filesystem::file_size(directory / "leaf.pid") == 0) {
            if (GetTickCount64() - started > 5000) {
                process.stop_and_reap();
                throw std::runtime_error("descendant did not become ready");
            }
            Sleep(10);
        }
        DWORD probe_pid = 0;
        std::ifstream(directory / "leaf.pid") >> probe_pid;
        HANDLE probe = OpenProcess(SYNCHRONIZE, FALSE, probe_pid);
        if (!probe && (scenario != L"short-probe" || GetLastError() != ERROR_INVALID_PARAMETER)) {
            throw std::runtime_error("could not observe the owned probe");
        }
        if (scenario == L"controller-exit") ExitProcess(0);
        try {
            const DWORD state = WaitForSingleObject(process.process(), scenario == L"short-probe" ? 2000 : 100);
            if (scenario == L"completed" || scenario == L"short-probe") {
                if (state != WAIT_OBJECT_0) throw std::runtime_error("script did not exit");
            } else {
                if (state != WAIT_TIMEOUT) throw std::runtime_error("script did not hold the probe pipe");
                throw Interrupted{}; // The timeout and MSI-cancel paths both unwind here.
            }
            process.stop_and_reap();
        } catch (const Interrupted&) {
            process.stop_and_reap();
        }
        // This is the journal-compensation boundary: eventual termination
        // after the controller has already returned would not be sufficient.
        const DWORD probe_state = probe ? WaitForSingleObject(probe, 0) : WAIT_OBJECT_0;
        if (probe) CloseHandle(probe);
        if (probe_state != WAIT_OBJECT_0) throw std::runtime_error("probe was still running at compensation");
        std::wcout << L"reaped " << scenario << L'\n';
        return 0;
    } catch (const convt::installer::ProcessError& error) {
        std::wcerr << error.operation() << L" (Windows error " << error.code() << L").\n";
    } catch (const std::exception& error) {
        std::cerr << error.what() << '\n';
    }
    return 3;
}
