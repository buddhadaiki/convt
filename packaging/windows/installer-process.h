#pragma once

#include <windows.h>

#include <memory>
#include <stdexcept>
#include <string>
#include <vector>

namespace convt::installer {

class ProcessError final : public std::runtime_error {
public:
    explicit ProcessError(const wchar_t* operation, DWORD code = GetLastError())
        : std::runtime_error("Installer process failed"), operation_(operation), code_(code) {}
    const wchar_t* operation() const noexcept { return operation_; }
    DWORD code() const noexcept { return code_; }
private:
    const wchar_t* operation_;
    DWORD code_;
};

// Shared by the MSI helper and the native process-lifecycle regression tests.
class ProcessTree final {
public:
    ProcessTree() {
        job_ = CreateJobObjectW(nullptr, nullptr);
        if (!job_) throw ProcessError(L"Could not create the installer process job");
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION limits{};
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if (!SetInformationJobObject(job_, JobObjectExtendedLimitInformation, &limits, sizeof(limits))) {
            const DWORD error = GetLastError();
            CloseHandle(job_);
            throw ProcessError(L"Could not configure the installer process job", error);
        }
        completion_ = CreateIoCompletionPort(INVALID_HANDLE_VALUE, nullptr, 0, 1);
        if (!completion_) {
            const DWORD error = GetLastError();
            CloseHandle(job_);
            throw ProcessError(L"Could not create the installer process completion port", error);
        }
        JOBOBJECT_ASSOCIATE_COMPLETION_PORT association{this, completion_};
        if (!SetInformationJobObject(job_, JobObjectAssociateCompletionPortInformation,
                                     &association, sizeof(association))) {
            const DWORD error = GetLastError();
            CloseHandle(completion_);
            CloseHandle(job_);
            throw ProcessError(L"Could not associate the installer process completion port", error);
        }
    }
    ~ProcessTree() {
        // The handle is not inherited. Controller termination also closes the
        // last job handle and kills helpers that are still holding probe pipes.
        CloseHandle(job_);
        CloseHandle(completion_);
        if (process_) CloseHandle(process_);
    }
    ProcessTree(const ProcessTree&) = delete;
    ProcessTree& operator=(const ProcessTree&) = delete;

    void start(const std::wstring& application, std::wstring command, DWORD flags,
               LPVOID environment, const wchar_t* directory, HANDLE user_token = nullptr) {
        if (process_) throw ProcessError(L"The installer process job was already started", ERROR_INVALID_FUNCTION);
        STARTUPINFOW startup{};
        startup.cb = sizeof(startup);
        PROCESS_INFORMATION created{};
        const BOOL launched = user_token ?
            CreateProcessAsUserW(user_token, application.c_str(), command.data(), nullptr, nullptr,
                                 FALSE, flags | CREATE_SUSPENDED, environment, directory, &startup, &created) :
            CreateProcessW(application.c_str(), command.data(), nullptr, nullptr, FALSE, flags | CREATE_SUSPENDED,
                           environment, directory, &startup, &created);
        if (!launched) throw ProcessError(L"Could not start the installer PowerShell helper");
        process_ = created.hProcess;
        // Assign before the first instruction: otherwise PowerShell can spawn
        // a Probe between CreateProcess and assignment and escape supervision.
        if (!AssignProcessToJobObject(job_, process_)) {
            const DWORD error = GetLastError();
            CloseHandle(created.hThread);
            if (!TerminateProcess(process_, ERROR_INSTALL_FAILURE)) {
                const DWORD stop_error = GetLastError();
                if (WaitForSingleObject(process_, 0) != WAIT_OBJECT_0) {
                    throw ProcessError(L"Could not stop the unassigned installer process", stop_error);
                }
            }
            const DWORD state = WaitForSingleObject(process_, reap_timeout_ms);
            if (state != WAIT_OBJECT_0) {
                throw ProcessError(L"Could not reap the unassigned installer process",
                                   state == WAIT_TIMEOUT ? ERROR_TIMEOUT : GetLastError());
            }
            throw ProcessError(L"Could not assign the installer PowerShell helper to its process job", error);
        }
        const DWORD resumed = ResumeThread(created.hThread);
        const DWORD error = GetLastError();
        CloseHandle(created.hThread);
        if (resumed == static_cast<DWORD>(-1)) {
            stop_and_reap();
            throw ProcessError(L"Could not resume the installer PowerShell helper", error);
        }
    }

    HANDLE process() const noexcept { return process_; }

    void stop_and_reap() {
        if (!process_ || reaped_) return;
        if (!TerminateJobObject(job_, ERROR_INSTALL_FAILURE)) {
            throw ProcessError(L"Could not stop the owned installer process tree");
        }
        // ActiveProcesses can become zero before a descendant handle is
        // signaled. Keep process references from the job's creation messages
        // and wait for those handles before permitting journal compensation.
        const ULONGLONG started = GetTickCount64();
        const DWORD state = WaitForSingleObject(process_, reap_timeout_ms);
        if (state != WAIT_OBJECT_0) {
            throw ProcessError(L"Could not reap the owned PowerShell process",
                               state == WAIT_TIMEOUT ? ERROR_TIMEOUT : GetLastError());
        }
        for (;;) {
            for (;;) {
                DWORD message = 0;
                ULONG_PTR key = 0;
                LPOVERLAPPED identifier = nullptr;
                if (!GetQueuedCompletionStatus(completion_, &message, &key, &identifier, 0)) {
                    const DWORD error = GetLastError();
                    if (error == WAIT_TIMEOUT) break;
                    throw ProcessError(L"Could not read installer process completion messages", error);
                }
                if (key != reinterpret_cast<ULONG_PTR>(this)) {
                    throw ProcessError(L"The installer process completion identity changed", ERROR_INVALID_DATA);
                }
                if (message == JOB_OBJECT_MSG_NEW_PROCESS) {
                    observe_process(static_cast<DWORD>(reinterpret_cast<ULONG_PTR>(identifier)));
                }
            }
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION accounting{};
            if (!QueryInformationJobObject(job_, JobObjectBasicAccountingInformation,
                                           &accounting, sizeof(accounting), nullptr)) {
                throw ProcessError(L"Could not inspect the owned installer process tree");
            }
            if (accounting.ActiveProcesses == 0 && observed_count_ == accounting.TotalProcesses) break;
            if (GetTickCount64() - started >= reap_timeout_ms) {
                // Job completion notifications are not guaranteed. A missing
                // creation message must fail closed, never permit early rollback.
                throw ProcessError(L"Could not confirm every owned installer process exited (completion messages missing)", ERROR_TIMEOUT);
            }
            Sleep(10);
        }
        for (const auto& observed : observed_processes_) {
            const ULONGLONG elapsed = GetTickCount64() - started;
            const DWORD remaining = elapsed >= reap_timeout_ms ? 0 : reap_timeout_ms - static_cast<DWORD>(elapsed);
            const DWORD exited = WaitForSingleObject(observed.get(), remaining);
            if (exited != WAIT_OBJECT_0) {
                throw ProcessError(L"Could not reap an owned installer descendant",
                                   exited == WAIT_TIMEOUT ? ERROR_TIMEOUT : GetLastError());
            }
        }
        reaped_ = true;
    }
private:
    void observe_process(DWORD identifier) {
        ProcessReference observed(OpenProcess(SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                                              FALSE, identifier), &CloseHandle);
        if (!observed) {
            const DWORD error = GetLastError();
            if (error != ERROR_INVALID_PARAMETER) {
                throw ProcessError(L"Could not observe an owned installer descendant", error);
            }
            // A process can finish before its creation message is consumed.
        } else {
            BOOL member = FALSE;
            if (!IsProcessInJob(observed.get(), job_, &member)) {
                throw ProcessError(L"Could not verify an owned installer descendant");
            }
            if (member) {
                observed_processes_.push_back(std::move(observed));
            } else if (WaitForSingleObject(observed.get(), 0) != WAIT_OBJECT_0) {
                // Job teardown and PID reuse are indistinguishable here.
                // Never block on an unrelated process or allow early rollback.
                throw ProcessError(L"Could not confirm an exited installer descendant's identity", ERROR_INVALID_DATA);
            }
        }
        ++observed_count_;
    }

    using ProcessReference = std::unique_ptr<void, decltype(&CloseHandle)>;
    static constexpr DWORD reap_timeout_ms = 30000;
    HANDLE job_ = nullptr;
    HANDLE completion_ = nullptr;
    HANDLE process_ = nullptr;
    std::vector<ProcessReference> observed_processes_;
    DWORD observed_count_ = 0;
    bool reaped_ = false;
};

} // namespace convt::installer
