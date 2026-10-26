#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

struct Fixture {
    root: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
    log: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("user home");
        let bin = root.path().join("commands");
        let log = root.path().join("systemctl.log");
        let state = root.path().join("state");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir(&bin).unwrap();
        fs::write(&state, "inactive").unwrap();
        write_executable(
            &bin.join("systemctl"),
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$SYSTEMCTL_LOG"
if [ "${FAKE_SYSTEMD_UNAVAILABLE:-}" = 1 ]; then
    printf '%s\n' 'Failed to connect to bus: No medium found' >&2
    exit 1
fi
case "$2" in
    show-environment|daemon-reload|enable) exit 0 ;;
    start) printf '%s' active > "$SYSTEMCTL_STATE" ;;
    stop) printf '%s' inactive > "$SYSTEMCTL_STATE" ;;
    show)
        if [ "${FAKE_SYSTEMD_MISSING:-}" = 1 ]; then
            printf '%s\n' 'LoadState=not-found' 'ActiveState=inactive' 'SubState=dead' 'MainPID=0'
            exit 1
        fi
        state=$(cat "$SYSTEMCTL_STATE")
        printf '%s\n' 'LoadState=loaded' "ActiveState=$state"
        if [ "$state" = active ]; then
            printf '%s\n' 'SubState=running' 'MainPID=1234'
        else
            printf '%s\n' 'SubState=dead' 'MainPID=0'
        fi
        ;;
    *) exit 3 ;;
esac
"#,
        );

        Self {
            root,
            home,
            bin,
            log,
            state,
        }
    }

    fn configure(
        &self,
        command: &mut Command,
    ) {
        let path = format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap());
        command
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("configuration"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("SYSTEMCTL_LOG", &self.log)
            .env("SYSTEMCTL_STATE", &self.state)
            .env_remove("FAKE_SYSTEMD_MISSING")
            .env_remove("FAKE_SYSTEMD_UNAVAILABLE")
            .env_remove("SEMMEM_BINARY");
    }

    fn cli(
        &self,
        action: &str,
    ) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_semmem"));
        self.configure(&mut command);
        command.args(["--json", action]);

        command
    }

    fn installer(&self) -> Command {
        let mut command = Command::new("sh");
        self.configure(&mut command);
        command.arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"));

        command
    }
}

fn write_executable(
    path: &Path,
    body: &str,
) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn decoded(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn lifecycle_runs_user_systemctl_and_returns_structured_status() {
    let fixture = Fixture::new();
    let inactive = decoded(fixture.cli("status").output().unwrap());
    assert_eq!(inactive["active_state"], "inactive");
    assert_eq!(inactive["main_pid"], 0);
    let active = decoded(fixture.cli("start").output().unwrap());
    assert_eq!(active["active_state"], "active");
    assert_eq!(active["sub_state"], "running");
    assert_eq!(active["main_pid"], 1234);
    let stopped = decoded(fixture.cli("stop").output().unwrap());
    assert_eq!(stopped["active_state"], "inactive");
    let log = fs::read_to_string(&fixture.log).unwrap();
    assert!(log.contains("--user start semmem.service"));
    assert!(log.contains("--user stop semmem.service"));
}

#[test]
fn missing_unit_and_unavailable_user_manager_have_actionable_errors() {
    let fixture = Fixture::new();
    let missing = fixture
        .cli("start")
        .env("FAKE_SYSTEMD_MISSING", "1")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("install.sh"));
    let unavailable = fixture
        .cli("status")
        .env("FAKE_SYSTEMD_UNAVAILABLE", "1")
        .output()
        .unwrap();
    assert!(!unavailable.status.success());
    let stderr = String::from_utf8_lossy(&unavailable.stderr);
    assert!(stderr.contains("Failed to connect to bus: No medium found"));
    assert!(stderr.contains("systemd user manager"));
}

#[test]
fn installer_preserves_configuration_and_data_and_enables_without_starting() {
    let fixture = Fixture::new();
    let source = fixture.root.path().join("prebuilt semmem");
    write_executable(&source, "#!/bin/sh\nexit 0\n");
    let configuration = fixture.home.join("configuration/semmem");
    fs::create_dir_all(&configuration).unwrap();
    let environment = configuration.join("environment");
    fs::write(
        &environment,
        "SEMMEM_ISSUER=https://identity.example.test\n",
    )
    .unwrap();
    let data = fixture.home.join("data/semmem");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("memory.sqlite3"), "preserve existing data").unwrap();
    let output = fixture
        .installer()
        .env("SEMMEM_BINARY", &source)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(fixture.home.join(".local/bin/semmem")).unwrap(),
        fs::read(source).unwrap()
    );
    assert_eq!(
        fs::read_to_string(&environment).unwrap(),
        "SEMMEM_ISSUER=https://identity.example.test\n"
    );
    assert_eq!(
        fs::read_to_string(data.join("memory.sqlite3")).unwrap(),
        "preserve existing data"
    );
    let unit = fs::read_to_string(
        fixture
            .home
            .join("configuration/systemd/user/semmem.service"),
    )
    .unwrap();
    assert!(unit.contains("ExecStart=/usr/bin/env \""));
    assert!(unit.contains("\" run --data-dir \""));
    assert!(unit.contains("EnvironmentFile=-/"));
    assert_eq!(
        fs::metadata(environment).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let log = fs::read_to_string(&fixture.log).unwrap();
    assert!(log.contains("--user daemon-reload"));
    assert!(log.contains("--user enable semmem.service"));
    assert!(!log.contains("--user start"));
}

#[test]
fn installer_builds_locked_release_binary_when_no_prebuilt_is_supplied() {
    let fixture = Fixture::new();
    write_executable(
        &fixture.bin.join("cargo"),
        r#"#!/bin/sh
printf '%s\n' "$*" > "$CARGO_LOG"
mkdir -p "$CARGO_TARGET_DIR/release"
printf '#!/bin/sh\nexit 0\n' > "$CARGO_TARGET_DIR/release/semmem"
chmod 0755 "$CARGO_TARGET_DIR/release/semmem"
"#,
    );
    let cargo_log = fixture.root.path().join("cargo.log");
    let output = fixture
        .installer()
        .env("CARGO_TARGET_DIR", fixture.root.path().join("build"))
        .env("CARGO_LOG", &cargo_log)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(cargo_log).unwrap().trim(),
        "build --locked --release --bin semmem"
    );
    assert!(fixture.home.join(".local/bin/semmem").is_file());
}

#[test]
fn installer_reports_missing_user_manager_before_installing() {
    let fixture = Fixture::new();
    let output = fixture
        .installer()
        .env("FAKE_SYSTEMD_UNAVAILABLE", "1")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("systemd user manager is unavailable")
    );
    assert!(!fixture.home.join(".local/bin/semmem").exists());
}
