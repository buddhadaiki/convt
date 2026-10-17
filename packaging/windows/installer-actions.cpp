#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <msi.h>
#include <msiquery.h>
#include <shlobj.h>
#include <objbase.h>
#include <userenv.h>

#include "installer-process.h"

#include <algorithm>
#include <cstdint>
#include <cwctype>
#include <exception>
#include <memory>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

static_assert(sizeof(void*) == 8, "The installer custom actions require x64.");

namespace {
HINSTANCE module_instance = nullptr;
constexpr DWORD process_timeout_ms = 180000;
constexpr size_t maximum_log_bytes = 512 * 1024;
constexpr size_t maximum_line_bytes = 16 * 1024;

class Failure final : public std::runtime_error {
public:
    explicit Failure(std::wstring message, UINT result = ERROR_INSTALL_FAILURE)
        : std::runtime_error("Installer action failed"), message_(std::move(message)), result_(result) {}
    const std::wstring& message() const noexcept { return message_; }
    UINT result() const noexcept { return result_; }
private:
    std::wstring message_;
    UINT result_;
};

[[noreturn]] void fail(const std::wstring& message, DWORD error = GetLastError()) {
    throw Failure(message + L" (Windows error " + std::to_wstring(error) + L").");
}

class Handle final {
public:
    explicit Handle(HANDLE handle = nullptr) noexcept : handle_(handle) {}
    ~Handle() { reset(); }
    Handle(const Handle&) = delete;
    Handle& operator=(const Handle&) = delete;
    Handle(Handle&& other) noexcept : handle_(other.release()) {}
    Handle& operator=(Handle&& other) noexcept {
        if (this != &other) reset(other.release());
        return *this;
    }
    HANDLE get() const noexcept { return handle_; }
    bool valid() const noexcept { return handle_ && handle_ != INVALID_HANDLE_VALUE; }
    HANDLE release() noexcept { return std::exchange(handle_, nullptr); }
    void reset(HANDLE handle = nullptr) noexcept {
        if (valid()) CloseHandle(handle_);
        handle_ = handle;
    }
private:
    HANDLE handle_;
};

class Record final {
public:
    explicit Record(UINT fields) : handle_(MsiCreateRecord(fields)) {
        if (!handle_) throw Failure(L"Could not create an installer message record.");
    }
    ~Record() { MsiCloseHandle(handle_); }
    MSIHANDLE get() const noexcept { return handle_; }
private:
    MSIHANDLE handle_;
};

void log_message(MSIHANDLE installer, INSTALLMESSAGE kind, const std::wstring& message) {
    Record record(1);
    MsiRecordSetStringW(record.get(), 0, L"[1]");
    MsiRecordSetStringW(record.get(), 1, message.c_str());
    // Logging is best effort: an error record must not hide the original failure.
    MsiProcessMessage(installer, kind, record.get());
}

void progress(MSIHANDLE installer, int operation, int ticks = 1, bool allow_cancel = true) {
    Record record(4);
    MsiRecordSetInteger(record.get(), 1, operation);
    MsiRecordSetInteger(record.get(), 2, ticks);
    MsiRecordSetInteger(record.get(), 3, 0);
    MsiRecordSetInteger(record.get(), 4, 0);
    if (MsiProcessMessage(installer, INSTALLMESSAGE_PROGRESS, record.get()) == IDCANCEL && allow_cancel) {
        throw Failure(L"The installer action was cancelled.", ERROR_INSTALL_USEREXIT);
    }
}

std::wstring property(MSIHANDLE installer, const wchar_t* name) {
    DWORD length = 0;
    wchar_t empty = L'\0';
    UINT result = MsiGetPropertyW(installer, name, &empty, &length);
    if (result != ERROR_SUCCESS && result != ERROR_MORE_DATA) {
        fail(L"Could not read installer property " + std::wstring(name), result);
    }
    if (length > 32767) throw Failure(L"Installer property is too long: " + std::wstring(name));
    std::vector<wchar_t> value(static_cast<size_t>(length) + 1, L'\0');
    DWORD capacity = static_cast<DWORD>(value.size());
    result = MsiGetPropertyW(installer, name, value.data(), &capacity);
    if (result != ERROR_SUCCESS) fail(L"Could not read installer property " + std::wstring(name), result);
    return std::wstring(value.data(), capacity);
}

void set_property(MSIHANDLE installer, const wchar_t* name, const wchar_t* value) {
    const UINT result = MsiSetPropertyW(installer, name, value);
    if (result != ERROR_SUCCESS) fail(L"Could not set installer property " + std::wstring(name), result);
}

std::wstring canonical_guid(std::wstring text) {
    if (text.size() == 38 && text.front() == L'{' && text.back() == L'}') {
        text = text.substr(1, 36);
    }
    if (text.size() != 36) throw Failure(L"ProductCode must be a GUID.");
    for (size_t index = 0; index < text.size(); ++index) {
        const wchar_t character = text[index];
        const bool separator = index == 8 || index == 13 || index == 18 || index == 23;
        const bool hexadecimal = (character >= L'0' && character <= L'9') ||
            (character >= L'a' && character <= L'f') || (character >= L'A' && character <= L'F');
        if (separator ? character != L'-' : !hexadecimal) {
            throw Failure(L"ProductCode must be a GUID.");
        }
    }
    if (std::all_of(text.begin(), text.end(), [](wchar_t value) { return value == L'0' || value == L'-'; })) {
        throw Failure(L"ProductCode must be a nonempty GUID.");
    }
    const std::wstring braced = L"{" + text + L"}";
    GUID guid{};
    if (FAILED(CLSIDFromString(braced.c_str(), &guid))) throw Failure(L"Invalid ProductCode GUID.");
    wchar_t canonical[39]{};
    if (!StringFromGUID2(guid, canonical, 39)) throw Failure(L"Could not normalize ProductCode.");
    text.assign(canonical + 1, 36);
    std::transform(text.begin(), text.end(), text.begin(), [](wchar_t value) {
        return static_cast<wchar_t>(std::towlower(value));
    });
    return text;
}

std::wstring new_transaction_guid() {
    GUID identifier{};
    const HRESULT generated = CoCreateGuid(&identifier);
    if (FAILED(generated)) fail(L"Could not generate the final configuration transaction", static_cast<DWORD>(generated));
    wchar_t braced[39]{};
    if (!StringFromGUID2(identifier, braced, 39)) throw Failure(L"Could not format the final configuration transaction.");
    return canonical_guid(braced);
}

std::wstring absolute_path(std::wstring path, bool optional = false) {
    if (path.empty() && optional) return path;
    if (path.empty()) throw Failure(L"InstallationRoot must be an absolute path.");
    for (wchar_t character : path) {
        if (character < L' ' || character == L'|' || character == L'"') {
            throw Failure(L"Installation paths contain an unsupported character.");
        }
    }
    std::replace(path.begin(), path.end(), L'/', L'\\');
    const bool drive = path.size() >= 3 &&
        ((path[0] >= L'A' && path[0] <= L'Z') || (path[0] >= L'a' && path[0] <= L'z')) &&
        path[1] == L':' && path[2] == L'\\';
    bool unc = path.size() > 4 && path[0] == L'\\' && path[1] == L'\\' &&
        path[2] != L'?' && path[2] != L'.';
    if (unc) {
        const size_t server_end = path.find(L'\\', 2);
        const size_t share_end = server_end == std::wstring::npos ? server_end : path.find(L'\\', server_end + 1);
        unc = server_end != std::wstring::npos && server_end > 2 &&
            server_end + 1 < path.size() && (share_end == std::wstring::npos || share_end > server_end + 1);
    }
    if (!drive && !unc) throw Failure(L"InstallationRoot must be an absolute drive or UNC path.");
    const DWORD length = GetFullPathNameW(path.c_str(), 0, nullptr, nullptr);
    if (!length || length > 32767) fail(L"Could not normalize the installation path");
    std::vector<wchar_t> normalized(static_cast<size_t>(length), L'\0');
    const DWORD written = GetFullPathNameW(path.c_str(), length, normalized.data(), nullptr);
    if (!written || written >= length) fail(L"Could not normalize the installation path");
    return std::wstring(normalized.data(), written);
}

std::wstring checkbox(const std::wstring& value) {
    if (value.empty() || value == L"0") return L"0";
    if (value == L"1") return value;
    throw Failure(L"Installer options must be empty, 0, or 1.");
}

struct Options {
    std::wstring transaction;
    std::wstring root;
    std::wstring previous_root;
    std::wstring context;
    std::wstring desktop;
};

struct EffectiveUser {
    Handle token;
    bool impersonated = false;

    EffectiveUser() {
        HANDLE thread_token = nullptr;
        if (OpenThreadToken(GetCurrentThread(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY,
                            TRUE, &thread_token)) {
            Handle original(thread_token);
            HANDLE primary = nullptr;
            if (!DuplicateTokenEx(original.get(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_ASSIGN_PRIMARY |
                                  TOKEN_IMPERSONATE, nullptr, SecurityImpersonation, TokenPrimary, &primary)) {
                fail(L"Could not duplicate the installer user's token");
            }
            token.reset(primary);
            impersonated = true;
        } else {
            const DWORD error = GetLastError();
            if (error != ERROR_NO_TOKEN) fail(L"Could not obtain the installer user's token", error);
            HANDLE process_token = nullptr;
            if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE | TOKEN_IMPERSONATE,
                                  &process_token)) {
                fail(L"Could not obtain the current user's token");
            }
            token.reset(process_token);
        }
        DWORD bytes = 0;
        GetTokenInformation(token.get(), TokenUser, nullptr, 0, &bytes);
        if (!bytes) fail(L"Could not inspect the installer user's identity");
        std::vector<BYTE> information(bytes);
        if (!GetTokenInformation(token.get(), TokenUser, information.data(), bytes, &bytes)) {
            fail(L"Could not inspect the installer user's identity");
        }
        const PSID sid = reinterpret_cast<const TOKEN_USER*>(information.data())->User.Sid;
        if (IsWellKnownSid(sid, WinLocalSystemSid) || IsWellKnownSid(sid, WinLocalServiceSid) ||
            IsWellKnownSid(sid, WinNetworkServiceSid)) {
            throw Failure(L"The per-user installer helper cannot run as a Windows service account.");
        }
    }
};

class UserEnvironment final {
public:
    explicit UserEnvironment(const EffectiveUser& user) {
        if (user.impersonated && !CreateEnvironmentBlock(&environment_, user.token.get(), FALSE)) {
            fail(L"Could not create the installer user's PowerShell environment");
        }
    }
    ~UserEnvironment() { if (environment_) DestroyEnvironmentBlock(environment_); }
    LPVOID get() const noexcept { return environment_; }
private:
    LPVOID environment_ = nullptr;
};

Options options(MSIHANDLE installer, bool immediate) {
    std::vector<std::wstring> fields;
    if (immediate) {
        for (const wchar_t* name : {L"ProductCode", L"INSTALLFOLDER", L"CONVT_PREVIOUS_INSTALLFOLDER",
                                   L"CONVT_CONTEXT_MENU", L"CONVT_DESKTOP_SHORTCUT"}) {
            fields.push_back(property(installer, name));
        }
    } else {
        const std::wstring data = property(installer, L"CustomActionData");
        size_t begin = 0;
        for (;;) {
            const size_t end = data.find(L'|', begin);
            fields.push_back(data.substr(begin, end == std::wstring::npos ? end : end - begin));
            if (end == std::wstring::npos) break;
            begin = end + 1;
        }
        if (fields.size() != 5) throw Failure(L"CustomActionData must contain exactly five fields.");
    }
    return {canonical_guid(fields[0]), absolute_path(fields[1]), absolute_path(fields[2], true),
            checkbox(fields[3]), checkbox(fields[4])};
}

void verify_regular(HANDLE file, const std::wstring& path, bool directory) {
    BY_HANDLE_FILE_INFORMATION information{};
    if (!GetFileInformationByHandle(file, &information)) fail(L"Could not inspect helper cache " + path);
    const DWORD attributes = information.dwFileAttributes;
    if ((attributes & FILE_ATTRIBUTE_REPARSE_POINT) ||
        static_cast<bool>(attributes & FILE_ATTRIBUTE_DIRECTORY) != directory) {
        throw Failure(L"Helper cache must not contain reparse points: " + path);
    }
}

// Keep every cache ancestor open without delete sharing, so a checked directory
// cannot be swapped for a junction before the scripts are opened or executed.
void pin_directory(const std::wstring& path, bool create, std::vector<Handle>& pins) {
    if (create && !CreateDirectoryW(path.c_str(), nullptr) && GetLastError() != ERROR_ALREADY_EXISTS) {
        fail(L"Could not create helper cache " + path);
    }
    Handle directory(CreateFileW(path.c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE,
                                 nullptr, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                                 nullptr));
    if (!directory.valid()) fail(L"Could not open helper cache " + path);
    verify_regular(directory.get(), path, true);
    pins.push_back(std::move(directory));
}

std::wstring helper_directory(const std::wstring& transaction, const EffectiveUser& user, std::vector<Handle>& pins) {
    PWSTR local = nullptr;
    const HRESULT result = SHGetKnownFolderPath(FOLDERID_LocalAppData, KF_FLAG_DEFAULT, user.token.get(), &local);
    std::unique_ptr<wchar_t, decltype(&CoTaskMemFree)> local_owner(local, &CoTaskMemFree);
    if (FAILED(result) || !local) fail(L"Could not locate per-user LocalAppData", static_cast<DWORD>(result));
    std::wstring path = absolute_path(local);
    while (path.size() > 3 && path.back() == L'\\') path.pop_back();
    size_t begin = 3;
    if (path[0] == L'\\') {
        const size_t server_end = path.find(L'\\', 2);
        const size_t share_end = path.find(L'\\', server_end + 1);
        begin = share_end == std::wstring::npos ? path.size() : share_end + 1;
    }
    pin_directory(path.substr(0, begin), false, pins);
    for (size_t end = path.find(L'\\', begin); ; end = path.find(L'\\', end + 1)) {
        pin_directory(path.substr(0, end), false, pins);
        if (end == std::wstring::npos) break;
    }
    for (const std::wstring& child : {std::wstring(L"Convt"), std::wstring(L"installer-helpers"), transaction}) {
        path += L"\\" + child;
        pin_directory(path, true, pins);
    }
    return path;
}

bool contents_equal(HANDLE file, const BYTE* expected, DWORD length) {
    LARGE_INTEGER size{};
    if (!GetFileSizeEx(file, &size)) fail(L"Could not inspect an embedded helper script");
    if (size.QuadPart != length) return false;
    LARGE_INTEGER start{};
    if (!SetFilePointerEx(file, start, nullptr, FILE_BEGIN)) fail(L"Could not read an embedded helper script");
    BYTE buffer[8192];
    DWORD offset = 0;
    while (offset < length) {
        DWORD read = 0;
        const DWORD count = (std::min)(length - offset, static_cast<DWORD>(sizeof(buffer)));
        if (!ReadFile(file, buffer, count, &read, nullptr)) fail(L"Could not read an embedded helper script");
        if (read != count || !std::equal(buffer, buffer + read, expected + offset)) return false;
        offset += read;
    }
    return true;
}

Handle extract_script(const std::wstring& directory, int identifier, const wchar_t* filename) {
    HRSRC resource = FindResourceW(module_instance, MAKEINTRESOURCEW(identifier), RT_RCDATA);
    if (!resource) fail(L"An embedded installer helper is missing");
    const DWORD length = SizeofResource(module_instance, resource);
    HGLOBAL loaded = LoadResource(module_instance, resource);
    const BYTE* bytes = loaded ? static_cast<const BYTE*>(LockResource(loaded)) : nullptr;
    if (!bytes || !length) fail(L"Could not read an embedded installer helper");
    const std::wstring path = directory + L"\\" + filename;
    Handle file(CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE | DELETE, 0, nullptr, OPEN_ALWAYS,
                           FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
    const DWORD creation = GetLastError();
    if (!file.valid()) fail(L"Could not extract installer helper " + path);
    verify_regular(file.get(), path, false);
    if (creation == ERROR_ALREADY_EXISTS) {
        if (!contents_equal(file.get(), bytes, length)) {
            throw Failure(L"The existing installer helper does not match this MSI: " + path);
        }
    } else {
        try {
            DWORD offset = 0;
            while (offset < length) {
                DWORD written = 0;
                if (!WriteFile(file.get(), bytes + offset, length - offset, &written, nullptr) || !written) {
                    fail(L"Could not write installer helper " + path);
                }
                offset += written;
            }
            if (!FlushFileBuffers(file.get())) fail(L"Could not save installer helper " + path);
        } catch (...) {
            // Delete only this newly created regular file through its owned
            // handle; leave every preexisting file intact on extraction failure.
            FILE_DISPOSITION_INFO disposition{TRUE};
            if (!SetFileInformationByHandle(file.get(), FileDispositionInfo, &disposition, sizeof(disposition))) {
                fail(L"Could not discard an incomplete installer helper " + path);
            }
            throw;
        }
    }
    file.reset();
    Handle pinned(CreateFileW(path.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
                             FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
    if (!pinned.valid()) fail(L"Could not secure installer helper " + path);
    verify_regular(pinned.get(), path, false);
    if (!contents_equal(pinned.get(), bytes, length)) throw Failure(L"Installer helper changed during extraction: " + path);
    return pinned;
}

std::wstring quote_argument(const std::wstring& argument) {
    std::wstring result = L"\"";
    size_t slashes = 0;
    for (wchar_t character : argument) {
        if (character == L'\\') {
            ++slashes;
        } else {
            result.append(character == L'"' ? slashes * 2 + 1 : slashes, L'\\');
            result.push_back(character);
            slashes = 0;
        }
    }
    result.append(slashes * 2, L'\\');
    result.push_back(L'"');
    return result;
}

std::wstring decode_output(const std::string& bytes) {
    if (bytes.empty()) return {};
    UINT codepage = CP_UTF8;
    DWORD flags = MB_ERR_INVALID_CHARS;
    int length = MultiByteToWideChar(codepage, flags, bytes.data(), static_cast<int>(bytes.size()), nullptr, 0);
    if (!length) {
        codepage = CP_OEMCP;
        flags = 0;
        length = MultiByteToWideChar(codepage, flags, bytes.data(), static_cast<int>(bytes.size()), nullptr, 0);
    }
    if (!length) return L"[Unable to decode PowerShell output]";
    std::wstring decoded(static_cast<size_t>(length), L'\0');
    MultiByteToWideChar(codepage, flags, bytes.data(), static_cast<int>(bytes.size()), decoded.data(), length);
    for (wchar_t& character : decoded) if (character < L' ' && character != L'\t') character = L' ';
    return decoded;
}

class ResultLog final {
public:
    explicit ResultLog(const std::wstring& directory) {
        GUID identifier{};
        const HRESULT generated = CoCreateGuid(&identifier);
        if (FAILED(generated)) fail(L"Could not generate an installer result identifier", static_cast<DWORD>(generated));
        wchar_t braced[39]{};
        if (!StringFromGUID2(identifier, braced, 39)) throw Failure(L"Could not format an installer result identifier.");
        std::wstring name = canonical_guid(braced);
        name.erase(std::remove(name.begin(), name.end(), L'-'), name.end());
        path_ = directory + L"\\result-" + name + L".log";
        // Attribute-only access allows PowerShell's WriteAllText to write the
        // precreated file while delete sharing prevents replacing its path.
        pin_.reset(CreateFileW(path_.c_str(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE,
                               nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
        if (!pin_.valid()) fail(L"Could not create the owned PowerShell result file");
        verify_regular(pin_.get(), path_, false);
        if (!GetFileInformationByHandle(pin_.get(), &identity_)) fail(L"Could not inspect the owned PowerShell result file");
    }
    ~ResultLog() {
        pin_.reset();
        Handle cleanup(CreateFileW(path_.c_str(), FILE_READ_ATTRIBUTES | DELETE, 0, nullptr, OPEN_EXISTING,
                                   FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
        BY_HANDLE_FILE_INFORMATION actual{};
        if (cleanup.valid() && GetFileInformationByHandle(cleanup.get(), &actual) &&
            !(actual.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY)) &&
            same_file(actual)) {
            FILE_DISPOSITION_INFO disposition{TRUE};
            // A failed cleanup retains only an inert log; it must not turn an
            // already completed integration into an installation failure.
            SetFileInformationByHandle(cleanup.get(), FileDispositionInfo, &disposition, sizeof(disposition));
        }
    }
    const std::wstring& path() const noexcept { return path_; }
    void emit(MSIHANDLE installer) {
        Handle file(CreateFileW(path_.c_str(), GENERIC_READ, FILE_SHARE_READ, nullptr, OPEN_EXISTING,
                                FILE_FLAG_OPEN_REPARSE_POINT, nullptr));
        if (!file.valid()) fail(L"Could not read the owned PowerShell result file");
        verify_regular(file.get(), path_, false);
        BY_HANDLE_FILE_INFORMATION actual{};
        if (!GetFileInformationByHandle(file.get(), &actual) || !same_file(actual)) {
            throw Failure(L"The owned PowerShell result file was replaced.");
        }
        std::string pending;
        size_t consumed = 0;
        while (consumed < maximum_log_bytes) {
            char buffer[4096];
            DWORD read = 0;
            if (!ReadFile(file.get(), buffer, sizeof(buffer), &read, nullptr)) fail(L"Could not read PowerShell result text");
            if (!read) break;
            consumed += read;
            for (DWORD index = 0; index < read; ++index) {
                if (buffer[index] == '\n') {
                    if (!pending.empty()) log_message(installer, INSTALLMESSAGE_INFO, L"convt PowerShell: " + decode_output(pending));
                    pending.clear();
                } else if (buffer[index] != '\r' && pending.size() < maximum_line_bytes) {
                    pending.push_back(buffer[index]);
                }
            }
        }
        if (!pending.empty()) log_message(installer, INSTALLMESSAGE_INFO, L"convt PowerShell: " + decode_output(pending));
        if (consumed >= maximum_log_bytes) log_message(installer, INSTALLMESSAGE_INFO, L"convt: further PowerShell result text omitted.");
    }
private:
    bool same_file(const BY_HANDLE_FILE_INFORMATION& actual) const noexcept {
        return actual.dwVolumeSerialNumber == identity_.dwVolumeSerialNumber &&
            actual.nFileIndexHigh == identity_.nFileIndexHigh && actual.nFileIndexLow == identity_.nFileIndexLow;
    }
    std::wstring path_;
    Handle pin_;
    BY_HANDLE_FILE_INFORMATION identity_{};
};

void run_script(MSIHANDLE installer, const wchar_t* action, const Options& selected, const EffectiveUser& user,
                const std::wstring& directory, bool allow_cancel = true, bool* child_reaped = nullptr) try {
    if (child_reaped) *child_reaped = false;
    wchar_t system_directory[MAX_PATH]{};
    const UINT length = GetSystemDirectoryW(system_directory, MAX_PATH);
    if (!length || length >= MAX_PATH) fail(L"Could not locate System32");
    const std::wstring powershell = std::wstring(system_directory) + L"\\WindowsPowerShell\\v1.0\\powershell.exe";
    ResultLog result_log(directory);
    // Windows clients default to the Restricted execution policy, which refuses
    // every -File script. Bypass applies to this process only; Group Policy
    // (MachinePolicy/UserPolicy) still takes precedence and needs signed helpers.
    std::vector<std::wstring> arguments{powershell, L"-NoProfile", L"-NonInteractive",
        L"-ExecutionPolicy", L"Bypass", L"-File",
        directory + L"\\finish-options.ps1", L"-Action", action, L"-InstallationRoot", selected.root,
        L"-ContextMenu", selected.context, L"-DesktopShortcut", selected.desktop,
        L"-TransactionId", selected.transaction, L"-CoreModernScript", directory + L"\\modern-menu.ps1",
        L"-ResultPath", result_log.path()};
    if (!selected.previous_root.empty()) {
        arguments.insert(arguments.end(), {L"-PreviousInstallationRoot", selected.previous_root});
    }
    if (std::wstring(action) == L"Apply" || std::wstring(action) == L"Remove" || std::wstring(action) == L"Configure") {
        arguments.push_back(L"-HoldTransaction");
    }
    std::wstring command;
    for (const auto& argument : arguments) {
        if (!command.empty()) command += L' ';
        command += quote_argument(argument);
    }
    if (command.size() >= 32767) throw Failure(L"Installer helper command exceeds the Windows command line limit.");
    UserEnvironment environment(user);
    DWORD flags = CREATE_NO_WINDOW;
    if (environment.get()) flags |= CREATE_UNICODE_ENVIRONMENT;
    // CreateProcess ignores a thread's impersonation token. Deferred MSI
    // actions must explicitly launch as that user, never as the MSI server.
    convt::installer::ProcessTree process;
    process.start(powershell, command, flags, environment.get(), directory.c_str(),
                  user.impersonated ? user.token.get() : nullptr);
    const ULONGLONG started = GetTickCount64();
    try {
        for (;;) {
            const DWORD state = WaitForSingleObject(process.process(), 1000);
            if (state == WAIT_OBJECT_0) break;
            if (state == WAIT_FAILED) fail(L"Could not wait for the installer PowerShell helper");
            if (GetTickCount64() - started >= process_timeout_ms) {
                throw Failure(L"The installer PowerShell helper exceeded the 180 second time limit.");
            }
            progress(installer, 2, 0, allow_cancel);
        }
    } catch (...) {
        process.stop_and_reap();
        if (child_reaped) *child_reaped = true;
        throw;
    }
    process.stop_and_reap();
    if (child_reaped) *child_reaped = true;
    result_log.emit(installer);
    DWORD exit_code = ERROR_INSTALL_FAILURE;
    if (!GetExitCodeProcess(process.process(), &exit_code)) fail(L"Could not read the installer PowerShell helper result");
    if (exit_code != ERROR_SUCCESS) {
        throw Failure(L"The installer PowerShell helper failed with exit code " + std::to_wstring(exit_code) + L".");
    }
} catch (const convt::installer::ProcessError& error) {
    fail(error.operation(), error.code());
}

UINT dispatch(MSIHANDLE installer, const wchar_t* action, bool immediate, bool configure = false) noexcept {
    try {
        if (std::wstring(action) == L"Prepare" && !MsiGetMode(installer, MSIRUNMODE_ROLLBACKENABLED)) {
            throw Failure(L"convt requires Windows Installer rollback to be enabled before preparing installation changes.");
        }
        if (configure) set_property(installer, L"CONVT_FINISH_APPLIED", L"0");
        Options selected = options(installer, immediate);
        if (configure) {
            // Final-page changes are a separate transaction after the MSI
            // lifecycle has committed. Never compensate its ProductCode journal.
            selected.transaction = new_transaction_guid();
        }
        const EffectiveUser user;
        std::vector<Handle> directory_pins;
        const std::wstring directory = helper_directory(selected.transaction, user, directory_pins);
        Handle modern = extract_script(directory, 101, L"modern-menu.ps1");
        Handle finish = extract_script(directory, 102, L"finish-options.ps1");
        log_message(installer, INSTALLMESSAGE_INFO, L"convt: running installer helper " + std::wstring(action) + L".");
        const bool allow_cancel = std::wstring(action) != L"Rollback" && std::wstring(action) != L"Commit";
        bool child_reaped = false;
        try {
            progress(installer, 3, 1, allow_cancel);
            run_script(installer, action, selected, user, directory, allow_cancel, &child_reaped);
            progress(installer, 2, 1, allow_cancel);
            if (configure) {
                set_property(installer, L"CONVT_FINISH_APPLIED", L"1");
                // Keep the journal through the last cancellable progress event
                // and the gate update. Commit is cleanup after that boundary.
                run_script(installer, L"Commit", selected, user, directory, false);
            }
        } catch (...) {
            const std::exception_ptr original = std::current_exception();
            std::wstring compensation_error;
            if (configure) {
                try { set_property(installer, L"CONVT_FINISH_APPLIED", L"0"); }
                catch (const Failure& reset_error) { compensation_error += L" Could not reset the configuration gate: " + reset_error.message(); }
                catch (...) { compensation_error += L" Could not reset the configuration gate."; }
            }
            if (configure && child_reaped) {
                // Configure runs after InstallFinalize. Its own journal is the
                // only rollback boundary, including cancellation or timeout.
                // run_script has already stopped/reaped the entire process job.
                try {
                    log_message(installer, INSTALLMESSAGE_INFO, L"convt: compensating failed final configuration.");
                    run_script(installer, L"Rollback", selected, user, directory, false);
                } catch (const Failure& rollback_error) {
                    compensation_error += L" Configuration rollback also failed: " + rollback_error.message();
                } catch (...) {
                    compensation_error += L" Configuration rollback also failed in the native helper.";
                }
            }
            try {
                std::rethrow_exception(original);
            } catch (const Failure& failure) {
                throw Failure(failure.message() + compensation_error, failure.result());
            } catch (const std::exception&) {
                throw Failure(L"convt: unexpected native installer helper failure." + compensation_error);
            } catch (...) {
                throw Failure(L"convt: unknown native installer helper failure." + compensation_error);
            }
        }
        return ERROR_SUCCESS;
    } catch (const Failure& error) {
        try { log_message(installer, INSTALLMESSAGE_ERROR, error.message()); } catch (...) {}
        return error.result();
    } catch (const std::exception&) {
        try { log_message(installer, INSTALLMESSAGE_ERROR, L"convt: unexpected native installer helper failure."); } catch (...) {}
    } catch (...) {
        try { log_message(installer, INSTALLMESSAGE_ERROR, L"convt: unknown native installer helper failure."); } catch (...) {}
    }
    return ERROR_INSTALL_FAILURE;
}
} // namespace

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) module_instance = instance;
    return TRUE;
}

extern "C" __declspec(dllexport) UINT __stdcall Prepare(MSIHANDLE installer) { return dispatch(installer, L"Prepare", true); }
extern "C" __declspec(dllexport) UINT __stdcall Apply(MSIHANDLE installer) { return dispatch(installer, L"Apply", false); }
extern "C" __declspec(dllexport) UINT __stdcall Rollback(MSIHANDLE installer) { return dispatch(installer, L"Rollback", false); }
extern "C" __declspec(dllexport) UINT __stdcall Commit(MSIHANDLE installer) { return dispatch(installer, L"Commit", false); }
extern "C" __declspec(dllexport) UINT __stdcall Remove(MSIHANDLE installer) { return dispatch(installer, L"Remove", false); }
extern "C" __declspec(dllexport) UINT __stdcall Configure(MSIHANDLE installer) { return dispatch(installer, L"Configure", true, true); }
