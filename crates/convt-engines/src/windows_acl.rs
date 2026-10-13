//! Fail-closed checks for executable document packs on local Windows filesystems.
use anyhow::bail;
use std::{
    ffi::c_void,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::Path,
    ptr,
    sync::OnceLock,
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, LocalFree},
    Security::{
        Authorization::{ConvertStringSidToSidW, GetSecurityInfo, SE_FILE_OBJECT},
        *,
    },
    Storage::FileSystem::{
        DELETE, FILE_APPEND_DATA, FILE_ATTRIBUTE_REPARSE_POINT, FILE_DELETE_CHILD,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FILE_WRITE_EA,
        READ_CONTROL, WRITE_DAC, WRITE_OWNER,
    },
    System::{
        SystemServices::{SECURITY_MANDATORY_MEDIUM_RID, SYSTEM_MANDATORY_LABEL_ACE_TYPE},
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: Windows allocated this descriptor with LocalAlloc.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn current_sid() -> anyhow::Result<Vec<u32>> {
    // SAFETY: valid output pointers; token buffer remains live until its SID is copied.
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut size = 0;
        GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut size);
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        );
        CloseHandle(token);
        if ok == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let length = GetLengthSid(user.User.Sid);
        let mut sid = vec![0u32; (length as usize).div_ceil(4)];
        if CopySid(length, sid.as_mut_ptr().cast(), user.User.Sid) == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(sid)
    }
}

fn system_sid(kind: WELL_KNOWN_SID_TYPE) -> anyhow::Result<Vec<u32>> {
    let mut sid = vec![0u32; 17];
    let mut size = 68;
    // SAFETY: buffer has SECURITY_MAX_SID_SIZE bytes and valid length pointer.
    if unsafe { CreateWellKnownSid(kind, ptr::null_mut(), sid.as_mut_ptr().cast(), &mut size) } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(sid)
}

struct TrustedSids {
    user: Vec<u32>,
    system: Vec<u32>,
    admins: Vec<u32>,
    installer: Vec<u32>,
    creator_owner: Vec<u32>,
}

fn trusted_sids() -> anyhow::Result<&'static TrustedSids> {
    static SIDS: OnceLock<Result<TrustedSids, String>> = OnceLock::new();
    SIDS.get_or_init(|| load_trusted_sids().map_err(|error| error.to_string()))
        .as_ref()
        .map_err(|error| anyhow::anyhow!("cannot obtain trusted Windows SIDs: {error}"))
}

fn load_trusted_sids() -> anyhow::Result<TrustedSids> {
    let user = current_sid()?;
    let system = system_sid(WinLocalSystemSid)?;
    let admins = system_sid(WinBuiltinAdministratorsSid)?;
    let creator_owner = system_sid(WinCreatorOwnerSid)?;
    // The drive root is normally owned by Windows Modules Installer.
    let installer_name: Vec<u16> = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut installer = ptr::null_mut();
    // SAFETY: valid NUL-terminated SID string and output pointer; LocalFree below.
    if unsafe { ConvertStringSidToSidW(installer_name.as_ptr(), &mut installer) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let descriptor = Descriptor(installer);
    // SAFETY: the Windows-allocated SID is valid until descriptor is dropped.
    let length = unsafe { GetLengthSid(installer) };
    let mut installer_sid = vec![0u32; (length as usize).div_ceil(4)];
    // SAFETY: destination holds the entire SID, and source is still live.
    if unsafe { CopySid(length, installer_sid.as_mut_ptr().cast(), installer) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    drop(descriptor);
    Ok(TrustedSids {
        user,
        system,
        admins,
        installer: installer_sid,
        creator_owner,
    })
}

/// Ancestors may be system-owned and allow users to create siblings, but may
/// not let another user replace an existing child. Pack objects permit our user and privileged system owners.
pub(crate) fn trusted(path: &Path, ancestor: bool) -> anyhow::Result<()> {
    // Open the object itself, without following its reparse tag, and inspect
    // the same handle's attributes and security descriptor.
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("document-pack reparse point forbidden: {}", path.display());
    }
    let mut owner = ptr::null_mut();
    let mut acl = ptr::null_mut();
    let mut label = ptr::null_mut();
    let mut descriptor = ptr::null_mut();
    // SAFETY: live object handle and valid out-pointers; descriptor owns returned SID/ACLs.
    // READ_CONTROL is enough to read the mandatory label.
    let error = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            &mut acl,
            &mut label,
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(std::io::Error::from_raw_os_error(error as i32).into());
    }
    let _descriptor = Descriptor(descriptor);
    if acl.is_null() || owner.is_null() {
        bail!(
            "document-pack path has no restrictive ACL: {}",
            path.display()
        );
    }
    // A process running at low integrity can write to an object labelled low
    // even when its DACL names only our user, so anything below medium is out.
    if !label.is_null() {
        // SAFETY: GetAce returns each ACE inside the live SACL; the label ACE
        // layout is checked by type before its SID is read.
        unsafe {
            for index in 0..u32::from((*label).AceCount) {
                let mut ace: *mut c_void = ptr::null_mut();
                if GetAce(label, index, &mut ace) == 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                if u32::from((*ace.cast::<ACE_HEADER>()).AceType) != SYSTEM_MANDATORY_LABEL_ACE_TYPE
                {
                    continue;
                }
                let entry = &*ace.cast::<SYSTEM_MANDATORY_LABEL_ACE>();
                let sid: PSID = ptr::addr_of!(entry.SidStart).cast_mut().cast();
                let count = *GetSidSubAuthorityCount(sid);
                let rid = if count == 0 {
                    0
                } else {
                    *GetSidSubAuthority(sid, u32::from(count) - 1)
                };
                if rid < SECURITY_MANDATORY_MEDIUM_RID as u32 {
                    bail!(
                        "document-pack path has a low integrity label: {}",
                        path.display()
                    );
                }
            }
        }
    }
    let sids = trusted_sids()?;

    let same = |a: PSID, b: &[u32]| {
        // SAFETY: all SID pointers are supplied by Windows security APIs.
        unsafe { EqualSid(a, b.as_ptr().cast_mut().cast()) != 0 }
    };
    let privileged =
        |sid| same(sid, &sids.user) || same(sid, &sids.system) || same(sid, &sids.admins);
    if !(privileged(owner) || (ancestor && same(owner, &sids.installer))) {
        bail!(
            "document-pack path has an untrusted owner: {}",
            path.display()
        );
    }
    let mut writes = DELETE
        | WRITE_DAC
        | WRITE_OWNER
        | FILE_DELETE_CHILD
        | FILE_WRITE_EA
        | FILE_WRITE_ATTRIBUTES
        | 0x10000000
        | 0x40000000;
    if !ancestor {
        writes |= FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_WRITE_EA | FILE_WRITE_ATTRIBUTES;
    }
    // SAFETY: GetAce returns each ACE inside the live security descriptor. Basic
    // allow/deny ACE layouts are checked before reading their SID and access mask.
    unsafe {
        for index in 0..u32::from((*acl).AceCount) {
            let mut ace: *mut c_void = ptr::null_mut();
            if GetAce(acl, index, &mut ace) == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let header = &*ace.cast::<ACE_HEADER>();
            let flags = u32::from(header.AceFlags);
            let inherit_only = flags & INHERIT_ONLY_ACE != 0;
            if inherit_only
                && (ancestor || flags & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE) == 0)
            {
                continue;
            }
            match header.AceType {
                0 => {
                    let allow = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                    let sid = ptr::addr_of!(allow.SidStart).cast_mut().cast();
                    // Inherit-only grants do not affect this object, but pack
                    // directories must not give new staging/extracted children
                    // unsafe permissions before their contents are verified.
                    // CREATOR OWNER becomes the trusted creating user on each
                    // child; its effective ACL is still checked individually.
                    if inherit_only && same(sid, &sids.creator_owner) {
                        continue;
                    }
                    if allow.Mask & writes != 0 && !privileged(sid) {
                        bail!(
                            "document-pack path is writable by another principal: {}",
                            path.display()
                        );
                    }
                }
                1 => {} // Denies cannot grant write permission.
                _ => bail!(
                    "document-pack ACL contains an unsupported ACE: {}",
                    path.display()
                ),
            }
        }
    }
    Ok(())
}

/// Test fixtures must not inherit grants from a shared TEMP or Desktop folder.
/// Only the fresh TempDir receives a new ACL; all existing ancestors are read-only.
#[cfg(test)]
pub(crate) fn private_test_dir() -> anyhow::Result<tempfile::TempDir> {
    let profile =
        dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Windows profile is unavailable"))?;
    private_test_dir_in(&profile)
}

#[cfg(test)]
fn private_test_dir_in(parent: &Path) -> anyhow::Result<tempfile::TempDir> {
    use anyhow::Context;
    use std::path::{Component, PathBuf};
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE, SET_ACCESS, SetEntriesInAclW, SetSecurityInfo,
        TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    if !parent.is_absolute() || !parent.is_dir() {
        bail!("Windows ACL fixture parent must be an existing absolute directory");
    }
    let mut path = PathBuf::new();
    for component in parent.components() {
        if matches!(component, Component::ParentDir) {
            bail!("Windows ACL fixture parent must not contain parent traversal");
        }
        path.push(component);
        if !matches!(component, Component::Prefix(_)) {
            trusted(&path, true).with_context(|| {
                format!("cannot inspect ACL fixture ancestor {}", path.display())
            })?;
        }
    }
    // A missing next component requires a private parent, not just an ancestor
    // that permits other users to create siblings. Refuse it before creating.
    trusted(parent, false)
        .with_context(|| format!("ACL fixture parent is not private: {}", parent.display()))?;
    let temp = tempfile::Builder::new()
        .prefix(".convt-acl-test-")
        .tempdir_in(parent)
        .with_context(|| format!("cannot create a fresh ACL fixture in {}", parent.display()))?;
    let sids = trusted_sids().context("cannot load trusted SIDs for the ACL fixture")?;
    let entries = [&sids.user, &sids.system, &sids.admins].map(|sid| EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: SUB_CONTAINERS_AND_OBJECTS_INHERIT,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid.as_ptr().cast_mut().cast(),
        },
    });
    let mut acl = ptr::null_mut();
    // SAFETY: the SID buffers live through both security calls; Windows owns
    // the returned ACL until the Descriptor frees it after SetSecurityInfo.
    let error = unsafe {
        SetEntriesInAclW(
            entries.len() as u32,
            entries.as_ptr(),
            ptr::null(),
            &mut acl,
        )
    };
    if error != 0 {
        return Err(std::io::Error::from_raw_os_error(error as i32))
            .context("cannot build the ACL fixture DACL");
    }
    let _acl = Descriptor(acl.cast());
    // Keep query rights while the security API changes inherited DACL protection.
    let directory = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(temp.path())
        .with_context(|| {
            format!(
                "cannot open new ACL fixture {} for READ_CONTROL | WRITE_DAC",
                temp.path().display()
            )
        })?;
    // SAFETY: this handle refers only to the directory just created above.
    // Protection removes ambient inherited grants; child grants remain private.
    let error = unsafe {
        SetSecurityInfo(
            directory.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            acl,
            ptr::null(),
        )
    };
    if error != 0 {
        return Err(std::io::Error::from_raw_os_error(error as i32)).with_context(|| {
            format!(
                "cannot protect the new ACL fixture DACL at {}",
                temp.path().display()
            )
        });
    }
    trusted(temp.path(), false)
        .with_context(|| format!("new ACL fixture is not private: {}", temp.path().display()))?;
    Ok(temp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn private_pack_rejects_other_users_write_access() {
        let temp = private_test_dir().unwrap();
        let file = temp.path().join("launcher.exe");
        std::fs::write(&file, b"not executable").unwrap();
        trusted(temp.path(), false).unwrap();
        trusted(&file, false).unwrap();
        let result = Command::new("icacls")
            .arg(&file)
            .args(["/grant", "*S-1-1-0:(M)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(trusted(&file, false).is_err());
    }

    #[test]
    fn ancestor_can_allow_sibling_creation_but_not_child_replacement() {
        let temp = private_test_dir().unwrap();
        let result = Command::new("icacls")
            .arg(temp.path())
            .args(["/grant", "*S-1-1-0:(AD)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        trusted(temp.path(), true).unwrap();
        assert!(trusted(temp.path(), false).is_err());
        let result = Command::new("icacls")
            .arg(temp.path())
            .args(["/grant", "*S-1-1-0:(DC)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(trusted(temp.path(), true).is_err());
    }

    #[test]
    fn inherit_only_creator_owner_does_not_grant_access_to_pack_directory() {
        let temp = private_test_dir().unwrap();
        let result = Command::new("icacls")
            .arg(temp.path())
            .args(["/grant", "*S-1-3-0:(OI)(CI)(IO)(F)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        trusted(temp.path(), false).unwrap();
        trusted(temp.path(), true).unwrap();
        let child = temp.path().join("child");
        std::fs::create_dir(&child).unwrap();
        trusted(&child, false).unwrap();
        let result = Command::new("icacls")
            .arg(&child)
            .args(["/grant", "*S-1-1-0:(M)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(trusted(&child, false).is_err());
    }

    #[test]
    fn inherit_only_other_users_cannot_make_new_pack_children_writable() {
        let temp = private_test_dir().unwrap();
        let result = Command::new("icacls")
            .arg(temp.path())
            .args(["/grant", "*S-1-1-0:(OI)(CI)(IO)(F)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        trusted(temp.path(), true).unwrap();
        assert!(trusted(temp.path(), false).is_err());
    }

    #[test]
    fn low_integrity_label_is_rejected() {
        let temp = private_test_dir().unwrap();
        let file = temp.path().join("soffice.exe");
        std::fs::write(&file, b"not executable").unwrap();
        trusted(&file, false).unwrap();
        let result = Command::new("icacls")
            .arg(&file)
            .args(["/setintegritylevel", "low"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(trusted(&file, false).is_err());
        assert!(trusted(&file, true).is_err());
    }

    #[test]
    fn junctions_are_rejected_without_following_them() {
        let temp = private_test_dir().unwrap();
        let real = temp.path().join("real");
        let junction = temp.path().join("junction");
        std::fs::create_dir(&real).unwrap();
        let result = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&real)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(trusted(&junction, false).is_err());
        std::fs::remove_dir(junction).unwrap();
    }

    #[test]
    fn private_fixture_children_are_trusted_and_unsafe_parents_are_refused() {
        let temp = private_test_dir().unwrap();
        let child = temp.path().join("child");
        std::fs::create_dir(&child).unwrap();
        let file = child.join("launcher.exe");
        std::fs::write(&file, b"not executable").unwrap();
        trusted(&child, false).unwrap();
        trusted(&file, false).unwrap();
        let result = Command::new("icacls")
            .arg(&child)
            .args(["/grant", "*S-1-1-0:(M)"])
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let error = private_test_dir_in(&child).unwrap_err();
        assert!(
            format!("{error:#}").contains("writable by another principal"),
            "{error:#}"
        );
        assert_eq!(std::fs::read_dir(&child).unwrap().count(), 1);
    }
}
