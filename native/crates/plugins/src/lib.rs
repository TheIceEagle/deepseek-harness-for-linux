//! Version 1 text extensions for the native client.
//!
//! Each call gets a fresh, fuel-metered Wasm instance with bounded linear memory
//! and no host imports. Plugins cannot access the filesystem, network, process
//! environment, UI objects, or other conversations. The host supplies only the
//! text the person explicitly chooses to transform.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use wasmi::{
    Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};

pub const API_VERSION: u32 = 1;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MEMORY_BYTES: usize = 8 * 1024 * 1024;
pub const EXECUTION_FUEL: u64 = 2_000_000;
const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MANIFEST_FILE: &str = "plugin.json";
const MODULE_FILE: &str = "plugin.wasm";
static NEXT_TEMPORARY: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub api_version: u32,
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    /// Short action label displayed by the native interface.
    pub command: String,
    pub module: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginError {
    Storage,
    InvalidPackage,
    InvalidManifest,
    InvalidModule,
    ImportsNotAllowed,
    AlreadyInstalled,
    NotFound,
    InvalidIdentifier,
    InputTooLarge,
    OutputTooLarge,
    ExecutionLimit,
    ExecutionFailed,
    InvalidOutput,
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Storage => "The plugin folder could not be accessed. Check its permissions and try again.",
            Self::InvalidPackage => "Choose a plugin folder containing regular plugin.json and plugin.wasm files.",
            Self::InvalidManifest => "The plugin manifest is invalid or uses an unsupported API version.",
            Self::InvalidModule => "The plugin does not provide the required version 1 text interface.",
            Self::ImportsNotAllowed => "This plugin requests system access that text plugins do not support.",
            Self::AlreadyInstalled => "A plugin with this identifier is already installed. Remove it before installing another version.",
            Self::NotFound => "This plugin is no longer installed. Refresh the plugin list and try again.",
            Self::InvalidIdentifier => "The plugin identifier is invalid.",
            Self::InputTooLarge => "The text is too large for this plugin. Use at most 64 KiB of text.",
            Self::OutputTooLarge => "The plugin returned more than 64 KiB of text. Your draft was kept.",
            Self::ExecutionLimit => "The plugin exceeded its execution budget. Your draft was kept.",
            Self::ExecutionFailed => "The plugin could not finish within its memory or execution limits. Your draft was kept.",
            Self::InvalidOutput => "The plugin returned invalid text. Your draft was kept.",
        })
    }
}

impl std::error::Error for PluginError {}

pub type Result<T> = std::result::Result<T, PluginError>;

#[derive(Clone)]
pub struct PluginManager {
    root: PathBuf,
    engine: Engine,
}

impl PluginManager {
    /// Creates a private plugin store beneath the native application's profile.
    pub fn new(profile_dir: impl AsRef<Path>) -> Result<Self> {
        private_directory(profile_dir.as_ref())?;
        let root = profile_dir.as_ref().join("plugins");
        private_directory(&root)?;
        let root = fs::canonicalize(root).map_err(|_| PluginError::Storage)?;
        let mut config = Config::default();
        config.consume_fuel(true);
        config.enforced_limits(wasmi::EnforcedLimits::strict());
        config.set_max_recursion_depth(128);
        config.set_max_stack_height(256 * 1024);
        config.set_max_cached_stacks(0);
        Ok(Self {
            root,
            engine: Engine::new(&config),
        })
    }

    pub fn list(&self) -> Result<Vec<PluginManifest>> {
        regular_directory(&self.root).map_err(|_| PluginError::Storage)?;
        let mut plugins = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|_| PluginError::Storage)? {
            let entry = entry.map_err(|_| PluginError::Storage)?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // Dot-prefixed staging/removal directories are never registrations.
            if !valid_identifier(&id) {
                continue;
            }
            plugins.push(self.installed_manifest(&id)?);
        }
        plugins.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        Ok(plugins)
    }

    /// Installs only the two declared files; no package scripts are executed.
    /// An existing identifier is never replaced implicitly.
    pub fn install(&self, source_folder: impl AsRef<Path>) -> Result<PluginManifest> {
        let source = source_folder.as_ref();
        regular_directory(source)?;
        let manifest = read_manifest(source)?;
        let bytes = read_regular_bounded(&source.join(MODULE_FILE), MAX_MODULE_BYTES)?;
        // Validate exports, start function, imports and allocation limits before
        // registering anything. Start functions have the same fuel budget.
        self.runner(&bytes)?;
        regular_directory(&self.root).map_err(|_| PluginError::Storage)?;
        let destination = self.root.join(&manifest.id);
        if fs::symlink_metadata(&destination).is_ok() {
            return Err(PluginError::AlreadyInstalled);
        }
        let staging = StagingDirectory::new(&self.root)?;
        write_private_file(
            &staging.0.join(MANIFEST_FILE),
            &serde_json::to_vec_pretty(&manifest).map_err(|_| PluginError::InvalidManifest)?,
        )?;
        write_private_file(&staging.0.join(MODULE_FILE), &bytes)?;
        fs::rename(&staging.0, &destination).map_err(|_| {
            if fs::symlink_metadata(&destination).is_ok() {
                PluginError::AlreadyInstalled
            } else {
                PluginError::Storage
            }
        })?;
        sync_directory(&self.root)?;
        Ok(manifest)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let source = self.installed_directory(id)?;
        let removal = temporary_path(&self.root, "remove");
        fs::rename(&source, &removal).map_err(|_| PluginError::Storage)?;
        fs::remove_dir_all(removal).map_err(|_| PluginError::Storage)?;
        sync_directory(&self.root)
    }

    /// Runs a pure text transform. The caller should replace its draft only on
    /// success; this function never writes the input or output to disk or logs.
    pub fn run(&self, id: &str, input: &str) -> Result<String> {
        if input.len() > MAX_TEXT_BYTES {
            return Err(PluginError::InputTooLarge);
        }
        self.installed_manifest(id)?;
        let bytes = read_regular_bounded(
            &self.installed_directory(id)?.join(MODULE_FILE),
            MAX_MODULE_BYTES,
        )?;
        let mut runner = self.runner(&bytes)?;
        let pointer = runner
            .alloc
            .call(&mut runner.store, input.len() as i32)
            .map_err(execution_error)? as u32;
        runner
            .memory
            .write(&mut runner.store, pointer as usize, input.as_bytes())
            .map_err(|_| PluginError::InvalidOutput)?;
        let result = runner
            .transform
            .call(&mut runner.store, (pointer as i32, input.len() as i32))
            .map_err(execution_error)? as u64;
        let output_pointer = (result >> 32) as u32 as usize;
        let output_length = result as u32 as usize;
        if output_length > MAX_TEXT_BYTES {
            return Err(PluginError::OutputTooLarge);
        }
        let mut output = vec![0; output_length];
        runner
            .memory
            .read(&runner.store, output_pointer, &mut output)
            .map_err(|_| PluginError::InvalidOutput)?;
        String::from_utf8(output).map_err(|_| PluginError::InvalidOutput)
    }

    fn installed_directory(&self, id: &str) -> Result<PathBuf> {
        if !valid_identifier(id) {
            return Err(PluginError::InvalidIdentifier);
        }
        regular_directory(&self.root).map_err(|_| PluginError::Storage)?;
        let path = self.root.join(id);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(PluginError::NotFound)
            }
            Err(_) => Err(PluginError::Storage),
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(path),
            Ok(_) => Err(PluginError::InvalidPackage),
        }
    }

    fn installed_manifest(&self, id: &str) -> Result<PluginManifest> {
        let manifest = read_manifest(&self.installed_directory(id)?)?;
        if manifest.id != id {
            return Err(PluginError::InvalidManifest);
        }
        Ok(manifest)
    }

    fn runner(&self, bytes: &[u8]) -> Result<Runner> {
        if !bytes.starts_with(b"\0asm") || bytes.len() > MAX_MODULE_BYTES {
            return Err(PluginError::InvalidModule);
        }
        let module = Module::new(&self.engine, bytes).map_err(|_| PluginError::InvalidModule)?;
        if module.imports().next().is_some() {
            return Err(PluginError::ImportsNotAllowed);
        }
        let limits = StoreLimitsBuilder::new()
            .memory_size(MAX_MEMORY_BYTES)
            .table_elements(1024)
            .memories(1)
            .tables(1)
            .instances(1)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        store
            .set_fuel(EXECUTION_FUEL)
            .map_err(|_| PluginError::ExecutionFailed)?;
        let instance = Linker::new(&self.engine)
            .instantiate_and_start(&mut store, &module)
            .map_err(execution_error)?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or(PluginError::InvalidModule)?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&store, "alloc")
            .map_err(|_| PluginError::InvalidModule)?;
        let transform = instance
            .get_typed_func::<(i32, i32), i64>(&store, "transform")
            .map_err(|_| PluginError::InvalidModule)?;
        Ok(Runner {
            store,
            memory,
            alloc,
            transform,
        })
    }
}

struct Runner {
    store: Store<StoreLimits>,
    memory: Memory,
    alloc: TypedFunc<i32, i32>,
    transform: TypedFunc<(i32, i32), i64>,
}

fn execution_error(error: wasmi::Error) -> PluginError {
    if error.as_trap_code() == Some(wasmi::TrapCode::OutOfFuel) {
        PluginError::ExecutionLimit
    } else {
        PluginError::ExecutionFailed
    }
}

fn valid_identifier(id: &str) -> bool {
    (2..=64).contains(&id.len())
        && id.as_bytes()[0].is_ascii_lowercase()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_label(value: &str, maximum: usize, empty_allowed: bool) -> bool {
    (empty_allowed || !value.trim().is_empty())
        && value.chars().count() <= maximum
        && !value.chars().any(char::is_control)
}

fn read_manifest(folder: &Path) -> Result<PluginManifest> {
    let bytes = read_regular_bounded(&folder.join(MANIFEST_FILE), MAX_MANIFEST_BYTES)?;
    let manifest: PluginManifest =
        serde_json::from_slice(&bytes).map_err(|_| PluginError::InvalidManifest)?;
    if manifest.api_version != API_VERSION
        || !valid_identifier(&manifest.id)
        || manifest.module != MODULE_FILE
        || !valid_label(&manifest.name, 80, false)
        || !valid_label(&manifest.command, 48, false)
        || !valid_label(&manifest.description, 256, true)
        || manifest.version.len() > 64
        || semver::Version::parse(&manifest.version).is_err()
    {
        return Err(PluginError::InvalidManifest);
    }
    Ok(manifest)
}

fn regular_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PluginError::InvalidPackage)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PluginError::InvalidPackage);
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_err() {
        fs::create_dir_all(path).map_err(|_| PluginError::Storage)?;
    }
    regular_directory(path).map_err(|_| PluginError::Storage)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| PluginError::Storage)
}

fn read_regular_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    // Open the final component without following links, then check the opened
    // file rather than trusting a separate path metadata lookup.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| PluginError::InvalidPackage)?;
    let metadata = file.metadata().map_err(|_| PluginError::InvalidPackage)?;
    if !metadata.is_file() || metadata.len() > maximum as u64 {
        return Err(PluginError::InvalidPackage);
    }
    let mut bytes = Vec::new();
    file.take((maximum + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| PluginError::InvalidPackage)?;
    if bytes.len() > maximum {
        return Err(PluginError::InvalidPackage);
    }
    Ok(bytes)
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| PluginError::Storage)?;
    file.write_all(bytes).map_err(|_| PluginError::Storage)?;
    file.sync_all().map_err(|_| PluginError::Storage)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| PluginError::Storage)
}

fn temporary_path(root: &Path, operation: &str) -> PathBuf {
    loop {
        let number = NEXT_TEMPORARY.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!(".{operation}-{}-{number}", std::process::id()));
        if fs::symlink_metadata(&path).is_err() {
            return path;
        }
    }
}

struct StagingDirectory(PathBuf);

impl StagingDirectory {
    fn new(root: &Path) -> Result<Self> {
        loop {
            let path = temporary_path(root, "install");
            match fs::create_dir(&path) {
                Ok(()) => {
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                        .map_err(|_| PluginError::Storage)?;
                    return Ok(Self(path));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(PluginError::Storage),
            }
        }
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
