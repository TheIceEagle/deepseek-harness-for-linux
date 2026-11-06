use harness_native_plugins::{PluginError, PluginManager, PluginManifest, MAX_TEXT_BYTES};
use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::PathBuf;
use tempfile::TempDir;

const UPPERCASE: &str = include_str!("../../../examples/uppercase/plugin.wat");

struct Fixture {
    directory: TempDir,
    source: PathBuf,
    profile: PathBuf,
    manager: PluginManager,
}

impl Fixture {
    fn new(module: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let profile = directory.path().join("profile");
        fs::create_dir(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            include_str!("../../../examples/uppercase/plugin.json"),
        )
        .unwrap();
        fs::write(source.join("plugin.wasm"), wat::parse_str(module).unwrap()).unwrap();
        let manager = PluginManager::new(&profile).unwrap();
        Self {
            directory,
            source,
            profile,
            manager,
        }
    }

    fn manifest(&self) -> PluginManifest {
        serde_json::from_slice(&fs::read(self.source.join("plugin.json")).unwrap()).unwrap()
    }

    fn write_manifest(&self, manifest: &PluginManifest) {
        fs::write(
            self.source.join("plugin.json"),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
    }
}

fn transform_module(body: &str) -> String {
    format!(
        "(module (memory (export \"memory\") 2 128)
         (func (export \"alloc\") (param i32) (result i32) i32.const 1024)
         (func (export \"transform\") (param i32 i32) (result i64) {body}))"
    )
}

#[test]
fn installs_runs_and_removes_a_real_wasm_text_plugin() {
    let fixture = Fixture::new(UPPERCASE);
    assert!(fixture.manager.list().unwrap().is_empty());
    let manifest = fixture.manager.install(&fixture.source).unwrap();
    assert_eq!(manifest.id, "uppercase");
    assert_eq!(fixture.manager.list().unwrap(), [manifest]);
    assert_eq!(
        fixture
            .manager
            .run("uppercase", "Hello, héllo 世界!")
            .unwrap(),
        "HELLO, HéLLO 世界!"
    );
    assert_eq!(fixture.manager.run("uppercase", "").unwrap(), "");
    assert_eq!(
        fixture
            .manager
            .run("uppercase", &"a".repeat(MAX_TEXT_BYTES))
            .unwrap(),
        "A".repeat(MAX_TEXT_BYTES)
    );
    // Installation copies the module, so removing the source package does not
    // turn an already installed extension into a dangling external reference.
    fs::remove_dir_all(&fixture.source).unwrap();
    assert_eq!(fixture.manager.run("uppercase", "again").unwrap(), "AGAIN");
    let reopened = PluginManager::new(&fixture.profile).unwrap();
    assert_eq!(reopened.run("uppercase", "saved").unwrap(), "SAVED");
    reopened.remove("uppercase").unwrap();
    assert!(reopened.list().unwrap().is_empty());
    assert_eq!(
        reopened.run("uppercase", "private draft").unwrap_err(),
        PluginError::NotFound
    );
}

#[test]
fn existing_plugin_is_never_replaced_implicitly() {
    let fixture = Fixture::new(UPPERCASE);
    let first = fixture.manager.install(&fixture.source).unwrap();
    let mut replacement = fixture.manifest();
    replacement.name = "Replacement".into();
    fixture.write_manifest(&replacement);
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::AlreadyInstalled
    );
    assert_eq!(fixture.manager.list().unwrap(), [first]);
}

#[test]
fn manifest_rejects_path_traversal_unknown_versions_and_undeclared_access() {
    let fixture = Fixture::new(UPPERCASE);
    let original = fixture.manifest();
    for invalid in ["../outside", "/tmp/outside", "has/slash", "UPPERCASE", "a"] {
        let mut manifest = original.clone();
        manifest.id = invalid.into();
        fixture.write_manifest(&manifest);
        assert_eq!(
            fixture.manager.install(&fixture.source).unwrap_err(),
            PluginError::InvalidManifest
        );
    }
    let mut manifest = original.clone();
    manifest.module = "../private-file".into();
    fixture.write_manifest(&manifest);
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::InvalidManifest
    );
    let mut manifest = original.clone();
    manifest.api_version = 2;
    fixture.write_manifest(&manifest);
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::InvalidManifest
    );
    let mut json = serde_json::to_value(original).unwrap();
    json["permissions"] = serde_json::json!(["filesystem"]);
    fs::write(
        fixture.source.join("plugin.json"),
        serde_json::to_vec(&json).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::InvalidManifest
    );
    assert!(fixture.manager.list().unwrap().is_empty());
}

#[test]
fn rejects_symlinked_packages_files_and_installed_directories() {
    let fixture = Fixture::new(UPPERCASE);
    let alias = fixture.directory.path().join("source-alias");
    symlink(&fixture.source, &alias).unwrap();
    assert_eq!(
        fixture.manager.install(alias).unwrap_err(),
        PluginError::InvalidPackage
    );
    let module = fixture.source.join("plugin.wasm");
    let external = fixture.directory.path().join("external.wasm");
    fs::rename(&module, &external).unwrap();
    symlink(&external, &module).unwrap();
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::InvalidPackage
    );
    let installed_alias = fixture.profile.join("plugins/linked");
    symlink(&fixture.source, &installed_alias).unwrap();
    assert_eq!(
        fixture.manager.remove("linked").unwrap_err(),
        PluginError::InvalidPackage
    );
    assert!(fixture.source.join("plugin.json").is_file());
    assert!(external.is_file());
}

#[test]
fn no_host_or_wasi_imports_are_available() {
    let module = "(module
        (import \"wasi_snapshot_preview1\" \"fd_write\" (func (param i32 i32 i32 i32) (result i32)))
        (memory (export \"memory\") 1)
        (func (export \"alloc\") (param i32) (result i32) i32.const 0)
        (func (export \"transform\") (param i32 i32) (result i64) i64.const 0))";
    let fixture = Fixture::new(module);
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::ImportsNotAllowed
    );
    assert!(fixture.manager.list().unwrap().is_empty());
}

#[test]
fn special_files_cannot_block_installation() {
    let fixture = Fixture::new(UPPERCASE);
    let module = fixture.source.join("plugin.wasm");
    fs::remove_file(&module).unwrap();
    let path = CString::new(module.as_os_str().as_bytes()).unwrap();
    // A package path is untrusted input. Opening a FIFO as a normal blocking
    // file would hang before the regular-file check could reject it.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::InvalidPackage
    );
    assert!(fixture.manager.list().unwrap().is_empty());
}

#[test]
fn excessive_module_structure_is_rejected_before_registration() {
    let globals = "(global i32 (i32.const 0))".repeat(1001);
    let module = format!(
        "(module {globals}
         (memory (export \"memory\") 1)
         (func (export \"alloc\") (param i32) (result i32) i32.const 0)
         (func (export \"transform\") (param i32 i32) (result i64) i64.const 0))"
    );
    let fixture = Fixture::new(&module);
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::InvalidModule
    );
    assert!(fixture.manager.list().unwrap().is_empty());
}

#[test]
fn runaway_transforms_and_start_functions_are_stopped_by_fuel() {
    let fixture = Fixture::new(&transform_module("(loop $forever br $forever) i64.const 0"));
    fixture.manager.install(&fixture.source).unwrap();
    assert_eq!(
        fixture.manager.run("uppercase", "draft").unwrap_err(),
        PluginError::ExecutionLimit
    );
    let start_loop = "(module
        (memory (export \"memory\") 1)
        (func $start (loop $forever br $forever)) (start $start)
        (func (export \"alloc\") (param i32) (result i32) i32.const 0)
        (func (export \"transform\") (param i32 i32) (result i64) i64.const 0))";
    let start_fixture = Fixture::new(start_loop);
    assert_eq!(
        start_fixture
            .manager
            .install(&start_fixture.source)
            .unwrap_err(),
        PluginError::ExecutionLimit
    );
    assert!(start_fixture.manager.list().unwrap().is_empty());
}

#[test]
fn initial_memory_and_growth_are_bounded() {
    let large = "(module
        (memory (export \"memory\") 129)
        (func (export \"alloc\") (param i32) (result i32) i32.const 0)
        (func (export \"transform\") (param i32 i32) (result i64) i64.const 0))";
    let fixture = Fixture::new(large);
    assert_eq!(
        fixture.manager.install(&fixture.source).unwrap_err(),
        PluginError::ExecutionFailed
    );
    let grow = "(module
        (memory (export \"memory\") 2)
        (func (export \"alloc\") (param i32) (result i32) i32.const 0)
        (func (export \"transform\") (param i32 i32) (result i64)
            i32.const 129 memory.grow drop i64.const 0))";
    let grow_fixture = Fixture::new(grow);
    grow_fixture.manager.install(&grow_fixture.source).unwrap();
    assert_eq!(
        grow_fixture.manager.run("uppercase", "draft").unwrap_err(),
        PluginError::ExecutionFailed
    );
}

#[test]
fn input_output_and_guest_memory_ranges_are_checked() {
    let fixture = Fixture::new(UPPERCASE);
    fixture.manager.install(&fixture.source).unwrap();
    assert_eq!(
        fixture
            .manager
            .run("uppercase", &"x".repeat(MAX_TEXT_BYTES + 1))
            .unwrap_err(),
        PluginError::InputTooLarge
    );
    let large_output = Fixture::new(&transform_module("i64.const 65537"));
    large_output.manager.install(&large_output.source).unwrap();
    assert_eq!(
        large_output.manager.run("uppercase", "draft").unwrap_err(),
        PluginError::OutputTooLarge
    );
    let invalid_pointer = Fixture::new(&transform_module("i64.const -4294967295"));
    invalid_pointer
        .manager
        .install(&invalid_pointer.source)
        .unwrap();
    assert_eq!(
        invalid_pointer
            .manager
            .run("uppercase", "draft")
            .unwrap_err(),
        PluginError::InvalidOutput
    );
}

#[test]
fn invalid_utf8_and_traps_do_not_expose_private_drafts() {
    let invalid_utf8 = "(module
        (memory (export \"memory\") 2)
        (data (i32.const 0) \"\\ff\")
        (func (export \"alloc\") (param i32) (result i32) i32.const 1024)
        (func (export \"transform\") (param i32 i32) (result i64) i64.const 1))";
    let fixture = Fixture::new(invalid_utf8);
    fixture.manager.install(&fixture.source).unwrap();
    let secret = "private-text-that-must-not-appear-in-errors";
    let error = fixture.manager.run("uppercase", secret).unwrap_err();
    assert_eq!(error, PluginError::InvalidOutput);
    assert!(!error.to_string().contains(secret));
    let trapped = Fixture::new(&transform_module("unreachable"));
    trapped.manager.install(&trapped.source).unwrap();
    let error = trapped.manager.run("uppercase", secret).unwrap_err();
    assert_eq!(error, PluginError::ExecutionFailed);
    assert!(!error.to_string().contains(secret));
}

#[test]
fn installed_manifest_identity_and_private_permissions_are_enforced() {
    let fixture = Fixture::new(UPPERCASE);
    fixture.manager.install(&fixture.source).unwrap();
    let directory = fixture.profile.join("plugins/uppercase");
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(directory.join("plugin.wasm"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut manifest = fixture.manifest();
    manifest.id = "different".into();
    fs::write(
        directory.join("plugin.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        fixture.manager.run("uppercase", "draft").unwrap_err(),
        PluginError::InvalidManifest
    );
    assert_eq!(
        fixture.manager.remove("../outside").unwrap_err(),
        PluginError::InvalidIdentifier
    );
}
