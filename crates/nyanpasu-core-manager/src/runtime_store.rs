//! Durable, manager-owned runtime configuration artifacts.

use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "test-hooks")]
use std::sync::{Arc, atomic::AtomicUsize};

use camino::{Utf8Path, Utf8PathBuf};
use tokio::io::AsyncWriteExt;

use crate::Error;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct RuntimeConfigStore {
    dir: Utf8PathBuf,
    #[cfg(feature = "test-hooks")]
    replace_parent_sync_failures: Arc<AtomicUsize>,
}

#[derive(Debug)]
pub(crate) struct RuntimeDirectoryLock {
    _file: std::fs::File,
}

#[derive(Debug)]
pub struct StagedRuntimeConfig {
    path: Utf8PathBuf,
    consumed: bool,
}

impl StagedRuntimeConfig {
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

impl Drop for StagedRuntimeConfig {
    fn drop(&mut self) {
        if !self.consumed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfigBackup {
    path: Utf8PathBuf,
    epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCommitDurability {
    Durable,
    Uncertain(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeConfigCommit {
    path: Utf8PathBuf,
    durability: RuntimeCommitDurability,
}

impl RuntimeConfigCommit {
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }

    pub fn durability(&self) -> &RuntimeCommitDurability {
        &self.durability
    }

    pub fn durability_warning(&self) -> Option<&str> {
        match &self.durability {
            RuntimeCommitDurability::Durable => None,
            RuntimeCommitDurability::Uncertain(warning) => Some(warning),
        }
    }
}

impl RuntimeConfigBackup {
    pub fn path(&self) -> &Utf8Path {
        &self.path
    }
}

impl RuntimeConfigStore {
    pub async fn new(dir: Utf8PathBuf) -> Result<Self, Error> {
        match tokio::fs::symlink_metadata(&dir).await {
            Ok(metadata) => validate_directory_metadata(&dir, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir_all(&dir).await?;
            }
            Err(error) => return Err(error.into()),
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).await?;
        }
        #[cfg(windows)]
        {
            harden_windows_directory_acl(&dir)?;
            verify_windows_directory_acl(&dir)?;
        }

        let canonical = tokio::fs::canonicalize(&dir).await?;
        let dir = Utf8PathBuf::from_path_buf(canonical)
            .map_err(|_| Error::InvalidManagerOptions("runtime directory is not UTF-8".into()))?;
        let metadata = tokio::fs::symlink_metadata(&dir).await?;
        validate_directory_metadata(&dir, &metadata)?;

        Ok(Self {
            dir,
            #[cfg(feature = "test-hooks")]
            replace_parent_sync_failures: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn dir(&self) -> &Utf8Path {
        &self.dir
    }

    pub fn runtime_path(&self, epoch: u64) -> Utf8PathBuf {
        self.dir.join(format!("config-{epoch}.yaml"))
    }

    pub fn pid_path(&self, epoch: u64) -> Utf8PathBuf {
        self.dir.join(format!("core-{epoch}.pid"))
    }

    pub fn socket_path(&self, epoch: u64) -> Utf8PathBuf {
        self.dir.join(format!("core-{epoch}.sock"))
    }

    #[cfg(feature = "test-hooks")]
    pub(crate) fn inject_replace_parent_sync_failure_once(&self) {
        self.replace_parent_sync_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) async fn acquire_ownership(&self) -> Result<RuntimeDirectoryLock, Error> {
        let path = self.dir.join(".manager.lock");
        tokio::task::spawn_blocking(move || acquire_runtime_directory_lock(&path))
            .await
            .map_err(std::io::Error::other)?
    }

    pub async fn stage(&self, epoch: u64, contents: &[u8]) -> Result<StagedRuntimeConfig, Error> {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = self.dir.join(format!(
            ".config-{epoch}.yaml.tmp-{}-{counter}",
            std::process::id()
        ));
        validate_absent_regular_target(&path).await?;

        let mut options = tokio::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).await?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .await?;
        }

        if let Err(error) = async {
            file.write_all(contents).await?;
            file.flush().await?;
            file.sync_all().await
        }
        .await
        {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
            return Err(error.into());
        }
        drop(file);
        Ok(StagedRuntimeConfig {
            path,
            consumed: false,
        })
    }

    pub async fn commit_new(
        &self,
        mut staged: StagedRuntimeConfig,
        epoch: u64,
    ) -> Result<Utf8PathBuf, Error> {
        self.validate_staged(&staged, epoch).await?;
        let target = self.runtime_path(epoch);
        validate_absent_regular_target(&target).await?;
        atomic_move_new(&staged.path, &target).await?;
        staged.consumed = true;
        sync_parent(&self.dir).await?;
        Ok(target)
    }

    pub async fn replace(&self, epoch: u64, contents: &[u8]) -> Result<RuntimeConfigCommit, Error> {
        let staged = self.stage(epoch, contents).await?;
        self.commit_replace(staged, epoch).await
    }

    /// Replaces the stable epoch file with bytes that were already staged and
    /// validated. The staged file remains in the runtime directory, so the
    /// atomicity guarantees are identical to [`Self::replace`].
    pub async fn commit_replace(
        &self,
        mut staged: StagedRuntimeConfig,
        epoch: u64,
    ) -> Result<RuntimeConfigCommit, Error> {
        self.validate_staged(&staged, epoch).await?;
        let target = self.runtime_path(epoch);
        validate_existing_regular_target(&target).await?;
        atomic_replace(&staged.path, &target).await?;
        staged.consumed = true;
        #[cfg(feature = "test-hooks")]
        let injected_failure = self
            .replace_parent_sync_failures
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok();
        #[cfg(feature = "test-hooks")]
        let parent_sync = if injected_failure {
            Err(std::io::Error::other(
                "injected parent-directory synchronization failure",
            ))
        } else {
            sync_parent(&self.dir).await
        };
        #[cfg(not(feature = "test-hooks"))]
        let parent_sync = sync_parent(&self.dir).await;
        Ok(installed_commit(target, parent_sync))
    }

    pub async fn backup(&self, epoch: u64, generation: u64) -> Result<RuntimeConfigBackup, Error> {
        let target = self.runtime_path(epoch);
        validate_existing_regular_target(&target).await?;
        let contents = tokio::fs::read(&target).await?;
        let mut staged = self.stage(epoch, &contents).await?;
        let backup_path = self
            .dir
            .join(format!("config-{epoch}.yaml.backup-{generation}"));
        validate_absent_regular_target(&backup_path).await?;
        atomic_move_new(&staged.path, &backup_path).await?;
        staged.consumed = true;
        sync_parent(&self.dir).await?;
        Ok(RuntimeConfigBackup {
            path: backup_path,
            epoch,
        })
    }

    pub async fn restore(
        &self,
        backup: &RuntimeConfigBackup,
    ) -> Result<RuntimeConfigCommit, Error> {
        validate_existing_regular_target(&backup.path).await?;
        let contents = tokio::fs::read(&backup.path).await?;
        self.replace(backup.epoch, &contents).await
    }

    pub async fn remove_backup(&self, backup: RuntimeConfigBackup) -> Result<(), Error> {
        remove_regular_file(&backup.path).await
    }

    pub async fn cleanup_epoch(&self, epoch: u64) -> Result<(), Error> {
        for path in [self.runtime_path(epoch), self.pid_path(epoch)] {
            remove_regular_file(&path).await?;
        }
        remove_socket_artifact(&self.socket_path(epoch)).await?;

        let prefix = format!("config-{epoch}.yaml.backup-");
        let temp_prefix = format!(".config-{epoch}.yaml.tmp-");
        let pid_temp_prefix = format!("core-{epoch}.pid.tmp-");
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name.starts_with(&prefix)
                || name.starts_with(&temp_prefix)
                || name.starts_with(&pid_temp_prefix)
            {
                let path = Utf8PathBuf::from_path_buf(entry.path())
                    .map_err(|_| Error::UnsafeRuntimeArtifact(self.dir.clone()))?;
                remove_regular_file(&path).await?;
            }
        }
        sync_parent(&self.dir).await?;
        Ok(())
    }

    pub async fn artifact_epochs(&self) -> Result<Vec<u64>, Error> {
        let mut epochs = Vec::new();
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if let Some(epoch) = artifact_epoch(&name) {
                epochs.push(epoch);
            }
        }
        epochs.sort_unstable();
        epochs.dedup();
        Ok(epochs)
    }

    async fn validate_staged(&self, staged: &StagedRuntimeConfig, epoch: u64) -> Result<(), Error> {
        if staged.path.parent() != Some(self.dir.as_path())
            || !staged
                .path
                .file_name()
                .is_some_and(|name| name.starts_with(&format!(".config-{epoch}.yaml.tmp-")))
        {
            return Err(Error::UnsafeRuntimeArtifact(staged.path.clone()));
        }
        validate_existing_regular_target(&staged.path).await
    }
}

fn acquire_runtime_directory_lock(path: &Utf8Path) -> Result<RuntimeDirectoryLock, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || is_reparse_point(&metadata) =>
        {
            return Err(Error::UnsafeRuntimeArtifact(path.to_owned()));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut options = std::fs::OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options.open(path).map_err(|error| {
        if runtime_lock_is_contended(&error) {
            Error::RuntimeDirectoryOwned(path.to_owned())
        } else {
            error.into()
        }
    })?;

    #[cfg(unix)]
    {
        use std::os::{fd::AsRawFd, raw::c_int};

        const LOCK_EX: c_int = 2;
        const LOCK_NB: c_int = 4;
        unsafe extern "C" {
            fn flock(fd: c_int, operation: c_int) -> c_int;
        }
        // SAFETY: file owns a valid descriptor for the duration of the call.
        if unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(if runtime_lock_is_contended(&error) {
                Error::RuntimeDirectoryOwned(path.to_owned())
            } else {
                error.into()
            });
        }
    }

    Ok(RuntimeDirectoryLock { _file: file })
}

#[cfg(unix)]
fn runtime_lock_is_contended(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::PermissionDenied
    )
}

#[cfg(windows)]
fn runtime_lock_is_contended(error: &std::io::Error) -> bool {
    matches!(error.raw_os_error(), Some(5 | 32 | 33))
}

fn artifact_epoch(name: &str) -> Option<u64> {
    let rest = name
        .strip_prefix("core-")
        .and_then(|value| value.strip_suffix(".pid"))
        .or_else(|| {
            name.strip_prefix("core-")
                .and_then(|value| value.strip_suffix(".sock"))
        })
        .or_else(|| {
            name.strip_prefix("config-").and_then(|value| {
                value
                    .strip_suffix(".yaml")
                    .or_else(|| value.split_once(".yaml.backup-").map(|(epoch, _)| epoch))
            })
        })
        .or_else(|| {
            name.strip_prefix(".config-")
                .and_then(|value| value.split_once(".yaml.tmp-").map(|(epoch, _)| epoch))
        })
        .or_else(|| {
            name.strip_prefix("core-")
                .and_then(|value| value.split_once(".pid.tmp-").map(|(epoch, _)| epoch))
        })?;
    rest.parse().ok()
}

fn installed_commit(path: Utf8PathBuf, parent_sync: std::io::Result<()>) -> RuntimeConfigCommit {
    let durability = match parent_sync {
        Ok(()) => RuntimeCommitDurability::Durable,
        Err(error) => RuntimeCommitDurability::Uncertain(format!(
            "runtime config was atomically installed, but parent-directory synchronization failed: {error}"
        )),
    };
    RuntimeConfigCommit { path, durability }
}

fn validate_directory_metadata(path: &Utf8Path, metadata: &std::fs::Metadata) -> Result<(), Error> {
    if metadata.file_type().is_symlink() || !metadata.is_dir() || is_reparse_point(metadata) {
        return Err(Error::UnsafeRuntimeArtifact(path.to_owned()));
    }
    Ok(())
}

async fn validate_absent_regular_target(path: &Utf8Path) -> Result<(), Error> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || is_reparse_point(&metadata) =>
        {
            Err(Error::UnsafeRuntimeArtifact(path.to_owned()))
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("runtime artifact already exists: {path}"),
        )
        .into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn validate_existing_regular_target(path: &Utf8Path) -> Result<(), Error> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || is_reparse_point(&metadata) {
        return Err(Error::UnsafeRuntimeArtifact(path.to_owned()));
    }
    Ok(())
}

async fn remove_regular_file(path: &Utf8Path) -> Result<(), Error> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || is_reparse_point(&metadata) =>
        {
            Err(Error::UnsafeRuntimeArtifact(path.to_owned()))
        }
        Ok(_) => {
            tokio::fs::remove_file(path).await?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

async fn remove_socket_artifact(path: &Utf8Path) -> Result<(), Error> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || is_reparse_point(&metadata) => {
            Err(Error::UnsafeRuntimeArtifact(path.to_owned()))
        }
        #[cfg(unix)]
        Ok(metadata) if !std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()) => {
            Err(Error::UnsafeRuntimeArtifact(path.to_owned()))
        }
        #[cfg(windows)]
        Ok(metadata) if !metadata.is_file() => Err(Error::UnsafeRuntimeArtifact(path.to_owned())),
        Ok(_) => {
            tokio::fs::remove_file(path).await?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
fn harden_windows_directory_acl(path: &Utf8Path) -> Result<(), Error> {
    use std::{iter, os::windows::ffi::OsStrExt};

    const SDDL_REVISION_1: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    #[link(name = "Advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            source: *const u16,
            revision: u32,
            descriptor: *mut *mut core::ffi::c_void,
            size: *mut u32,
        ) -> i32;
        fn SetFileSecurityW(
            file_name: *const u16,
            information: u32,
            descriptor: *mut core::ffi::c_void,
        ) -> i32;
    }
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
        .encode_utf16()
        .chain(iter::once(0))
        .collect();
    let path: Vec<u16> = path
        .as_std_path()
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: the SDDL string is valid, NUL-terminated UTF-16 and the output
    // pointer remains owned by LocalAlloc until LocalFree below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: both path and descriptor are valid for the duration of the call.
    let result = unsafe { SetFileSecurityW(path.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
    // SAFETY: descriptor was allocated by the conversion API above.
    unsafe { LocalFree(descriptor) };
    if result == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn verify_windows_directory_acl(path: &Utf8Path) -> Result<(), Error> {
    use std::{iter, os::windows::ffi::OsStrExt};

    const SDDL_REVISION_1: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const SE_DACL_PROTECTED: u16 = 0x1000;
    #[link(name = "Advapi32")]
    unsafe extern "system" {
        fn GetFileSecurityW(
            file_name: *const u16,
            information: u32,
            descriptor: *mut core::ffi::c_void,
            length: u32,
            needed: *mut u32,
        ) -> i32;
        fn GetSecurityDescriptorControl(
            descriptor: *const core::ffi::c_void,
            control: *mut u16,
            revision: *mut u32,
        ) -> i32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor: *const core::ffi::c_void,
            revision: u32,
            information: u32,
            output: *mut *mut u16,
            length: *mut u32,
        ) -> i32;
    }
    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }

    let path_wide: Vec<u16> = path
        .as_std_path()
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let mut needed = 0_u32;
    // SAFETY: null output with length zero is the documented size query.
    unsafe {
        GetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
    }
    if needed == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let words = (needed as usize).div_ceil(std::mem::size_of::<usize>());
    let mut descriptor = vec![0_usize; words];
    // SAFETY: descriptor has at least `needed` writable bytes and path is NUL-terminated.
    if unsafe {
        GetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION,
            descriptor.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }

    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: descriptor contains a security descriptor returned by GetFileSecurityW.
    if unsafe {
        GetSecurityDescriptorControl(descriptor.as_ptr().cast(), &mut control, &mut revision)
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(Error::UnsafeRuntimeArtifact(path.to_owned()));
    }

    let mut sddl = std::ptr::null_mut();
    let mut sddl_len = 0_u32;
    // SAFETY: descriptor is valid; the output is LocalAlloc-owned on success.
    if unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor.as_ptr().cast(),
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut sddl,
            &mut sddl_len,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: conversion returned `sddl_len` initialized UTF-16 code units.
    let text =
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sddl, sddl_len as usize) });
    // SAFETY: sddl was allocated by the conversion API above.
    unsafe { LocalFree(sddl.cast()) };
    let broad_principals = [";;;WD)", ";;;AU)", ";;;BU)", ";;;IU)", ";;;AN)", ";;;NU)"];
    if broad_principals
        .iter()
        .any(|principal| text.contains(principal))
    {
        return Err(Error::UnsafeRuntimeArtifact(path.to_owned()));
    }
    Ok(())
}

#[cfg(unix)]
async fn atomic_move_new(source: &Utf8Path, target: &Utf8Path) -> std::io::Result<()> {
    // Linking publishes the already-fsynced inode atomically and fails if the
    // destination appeared after validation; removing the staging name does
    // not affect readers of the committed target.
    tokio::fs::hard_link(source, target).await?;
    tokio::fs::remove_file(source).await
}

#[cfg(unix)]
async fn atomic_replace(source: &Utf8Path, target: &Utf8Path) -> std::io::Result<()> {
    tokio::fs::rename(source, target).await
}

#[cfg(windows)]
async fn atomic_move_new(source: &Utf8Path, target: &Utf8Path) -> std::io::Result<()> {
    windows_move_file(source, target, false)
}

#[cfg(windows)]
async fn atomic_replace(source: &Utf8Path, target: &Utf8Path) -> std::io::Result<()> {
    const RETRIES: usize = 20;
    for attempt in 0..RETRIES {
        match windows_replace_file(source, target) {
            Ok(()) => return Ok(()),
            Err(error)
                if matches!(error.raw_os_error(), Some(5 | 32 | 33)) && attempt + 1 < RETRIES =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded retry loop returns on its final attempt")
}

#[cfg(windows)]
fn windows_move_file(source: &Utf8Path, target: &Utf8Path, replace: bool) -> std::io::Result<()> {
    use std::{iter, os::windows::ffi::OsStrExt};

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source
        .as_std_path()
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let target: Vec<u16> = target
        .as_std_path()
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    // SAFETY: both arguments are valid, NUL-terminated UTF-16 strings for the duration of the call.
    if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), flags) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn windows_replace_file(source: &Utf8Path, target: &Utf8Path) -> std::io::Result<()> {
    use std::{iter, os::windows::ffi::OsStrExt};

    const REPLACEFILE_WRITE_THROUGH: u32 = 0x1;
    unsafe extern "system" {
        fn ReplaceFileW(
            replaced: *const u16,
            replacement: *const u16,
            backup: *const u16,
            flags: u32,
            exclude: *mut core::ffi::c_void,
            reserved: *mut core::ffi::c_void,
        ) -> i32;
    }
    let source: Vec<u16> = source
        .as_std_path()
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let target: Vec<u16> = target
        .as_std_path()
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    // SAFETY: path arguments are valid NUL-terminated UTF-16 strings; optional pointers are null.
    if unsafe {
        ReplaceFileW(
            target.as_ptr(),
            source.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
async fn sync_parent(dir: &Utf8Path) -> std::io::Result<()> {
    let dir = dir.to_owned();
    tokio::task::spawn_blocking(move || std::fs::File::open(dir)?.sync_all())
        .await
        .map_err(std::io::Error::other)?
}

#[cfg(windows)]
async fn sync_parent(_dir: &Utf8Path) -> std::io::Result<()> {
    // MoveFileExW requests write-through for first publication. Microsoft
    // documents ReplaceFileW's WRITE_THROUGH flag as unsupported, and std
    // cannot fsync a Windows directory, so replacement power-loss durability
    // is not asserted here.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store_dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = Utf8PathBuf::from_path_buf(dir.path().join("runtime")).unwrap();
        (dir, path)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn atomic_replace_readers_only_observe_complete_documents() {
        let (_guard, dir) = temp_store_dir();
        let store = RuntimeConfigStore::new(dir).await.unwrap();
        let old = b"marker: old\npayload: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n";
        let new = b"marker: new\npayload: bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n";
        let staged = store.stage(7, old).await.unwrap();
        let path = store.commit_new(staged, 7).await.unwrap();

        let mut readers = Vec::new();
        for _ in 0..8 {
            let path = path.clone();
            let old = old.to_vec();
            let new = new.to_vec();
            readers.push(tokio::spawn(async move {
                for _ in 0..500 {
                    #[cfg(windows)]
                    let observed = loop {
                        match tokio::fs::read(&path).await {
                            Ok(observed) => break observed,
                            Err(error) if matches!(error.raw_os_error(), Some(2 | 5 | 32 | 33)) => {
                                tokio::task::yield_now().await;
                            }
                            Err(error) => panic!("runtime read failed: {error}"),
                        }
                    };
                    #[cfg(not(windows))]
                    let observed = tokio::fs::read(&path)
                        .await
                        .unwrap_or_else(|error| panic!("runtime read failed: {error}"));
                    assert!(observed == old || observed == new, "partial YAML observed");
                    tokio::task::yield_now().await;
                }
            }));
        }
        for round in 0..100 {
            store
                .replace(7, if round % 2 == 0 { new } else { old })
                .await
                .unwrap();
        }
        for reader in readers {
            reader.await.unwrap();
        }
    }

    #[tokio::test]
    async fn backup_restore_and_cleanup_preserve_complete_versions() {
        let (_guard, dir) = temp_store_dir();
        let store = RuntimeConfigStore::new(dir).await.unwrap();
        let staged = store.stage(3, b"value: old\n").await.unwrap();
        let path = store.commit_new(staged, 3).await.unwrap();
        let backup = store.backup(3, 1).await.unwrap();
        store.replace(3, b"value: new\n").await.unwrap();
        store.restore(&backup).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            "value: old\n"
        );
        store.remove_backup(backup).await.unwrap();
        store.cleanup_epoch(3).await.unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_epoch_removes_a_real_unix_socket() {
        use std::os::unix::{fs::FileTypeExt, net::UnixListener};

        let (_guard, dir) = temp_store_dir();
        let store = RuntimeConfigStore::new(dir).await.unwrap();
        let socket_path = store.socket_path(9);
        let listener = UnixListener::bind(&socket_path).unwrap();
        assert!(
            std::fs::symlink_metadata(&socket_path)
                .unwrap()
                .file_type()
                .is_socket()
        );
        drop(listener);

        store.cleanup_epoch(9).await.unwrap();
        assert!(!socket_path.exists());
    }

    #[test]
    fn installed_commit_reports_parent_sync_uncertainty_without_becoming_an_error() {
        let path = Utf8PathBuf::from("config-4.yaml");
        let commit = installed_commit(
            path.clone(),
            Err(std::io::Error::other("injected directory fsync failure")),
        );
        assert_eq!(commit.path(), path);
        assert!(matches!(
            commit.durability(),
            RuntimeCommitDurability::Uncertain(message)
                if message.contains("atomically installed") && message.contains("injected")
        ));
    }
}
