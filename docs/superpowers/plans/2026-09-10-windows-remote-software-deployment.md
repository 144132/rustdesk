# Windows 远程软件部署与自动自愈实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在新育智慧校园 Windows 客户端中实现授权远程下载、检测、静默安装以及软件被删除后的自动恢复。

**Architecture:** 将清单校验、域名白名单、SHA-256 校验、安装命令构造和退避策略放在可测试的 Rust 核心模块中；将 Windows 下载、注册表/路径检测、服务态执行和状态文件放在 Windows 平台模块中。通过现有 RustDesk protobuf 网络消息传输结构化安装请求和状态，Flutter 只负责表单、权限可见性和状态展示，不直接执行命令。

**Tech Stack:** Rust、Tokio、Reqwest、URL、SHA-256、winreg、Windows Service、RustDesk protobuf、Flutter/Dart、GitHub Actions Windows runner。

**Spec:** `docs/superpowers/specs/2026-09-10-windows-remote-software-deployment-design.md`

## Global Constraints

- 第一版只支持 Windows；Android、macOS、Linux 不增加安装行为。
- 仅支持 `.msi` 和 `.exe`；不执行 PowerShell、CMD、BAT、脚本或任意 Shell 命令。
- URL 必须是 HTTPS，hostname 必须精确为 `szxinyu.com` 或其子域名，默认只允许 443 端口；禁止通过字符串后缀误放行 `evil-szxinyu.com`。
- HTTP 重定向最多跟随 5 次，并在每次跳转和最终地址重新校验白名单。
- 每个远程安装请求必须带 64 位十六进制 SHA-256；摘要不匹配时不安装。
- 远程安装必须同时经过正常会话授权、独立的软件安装权限、目标端策略和已运行的 RustDesk Windows 服务检查。
- 目标端缓存和清单位于 `%ProgramData%\\新育智慧校园\\remote-software`，使用 `.part` 和原子改名避免半成品。
- 自愈只恢复已登记软件的缺失状态，不自动卸载软件，不在第一版实现批量编排或版本升级。
- 每个任务使用 `request_id` 幂等，同一目标同时只运行一个软件安装任务。
- 每个行为先写失败测试并在可用环境中实际运行确认失败，再写最小实现；完成前必须在可用环境运行完整验证命令。本机缺少工具链时记录限制，并由 Task 6 的 GitHub Actions 在最终验收前补跑，不得把未运行报告为通过。

## 文件与职责地图

- `src/remote_software.rs`：跨平台清单类型、URL/SHA-256/字段校验、安装类型、检测规则、退出码和退避策略；不访问 Windows 注册表、不下载、不启动进程。
- `src/platform/windows_remote_software.rs`：Windows 下载器、重定向校验、缓存、注册表/路径检测、无 Shell 子进程执行、状态文件和自愈循环。
- `libs/hbb_common/protos/rendezvous.proto`：增加独立的软件安装控制权限。
- `libs/hbb_common/protos/message.proto`：增加安装请求、取消和状态消息及 `Message` union 字段。
- `src/server/connection.rs`：目标端鉴权/权限/消息范围检查，调用 Windows 安装服务并回传状态。
- `src/client.rs`、`src/client/io_loop.rs`、`src/flutter.rs`、`src/flutter_ffi.rs`：控制端发送请求、接收能力和状态、向 Flutter 暴露接口。
- `flutter/lib/models/model.dart`、`flutter/lib/desktop/pages/remote_page.dart`、`flutter/lib/desktop/widgets/remote_toolbar.dart`：读取目标能力、显示入口、发送清单和显示状态。
- `flutter/lib/desktop/widgets/remote_software_install_dialog.dart`：安装表单和字段校验。
- `flutter/test/remote_software_install_test.dart`、Rust 模块测试：覆盖纯逻辑和 UI 规则。
- `.github/workflows/ci.yml`、`.github/workflows/flutter-build.yml`：加入可重复的策略测试、Flutter 测试和 Windows 构建验证。

### Task 1: 建立清单、白名单和安装策略核心

**Files:**
- Create: `src/remote_software.rs`
- Modify: `src/lib.rs`
- Modify: `libs/hbb_common/src/config.rs`
- Test: `src/remote_software.rs` 内的 `#[cfg(test)]` 模块

**Interfaces:**
- Produces `InstallerType::{Msi, Exe}`、`DetectionRule::{MsiProductCode, UninstallDisplayName, ExePath}` 和 `InstallMode::{DownloadOnly, DownloadAndInstall}`。
- Produces `RemoteSoftwareManifest { request_id, software_name, package_url, sha256, installer_type, detection_rule, silent_args, mode }`，实现 `Clone + Debug + Serialize + Deserialize`。
- Produces `validate_package_url(&str) -> Result<url::Url, RemoteSoftwareError>`。
- Produces `validate_sha256(&str) -> Result<[u8; 32], RemoteSoftwareError>`。
- Produces `validate_manifest(&RemoteSoftwareManifest) -> Result<(), RemoteSoftwareError>`。
- Produces `RemoteSoftwareStage::{Queued, Downloading, Verifying, Detecting, Installing, AlreadyInstalled, Success, NeedsReboot, Failed}` and `RemoteSoftwareStatus { request_id, stage, message, exit_code, needs_reboot, progress_percent }`。
- Produces `InstallOutcome::{Success, NeedsReboot, Failed}`、`installer_outcome(Option<i32>) -> InstallOutcome` and `RemoteSoftwareStatus::success(&str, InstallOutcome)`。
- Produces `retry_delay(attempt: u32) -> Option<std::time::Duration>`，前三次分别返回 5、15、30 分钟，超过三次返回 `None` 表示暂停到下次服务重启。

- [ ] **Step 1: Write the failing policy tests**

```rust
#[test]
fn package_url_accepts_root_and_subdomains_but_rejects_lookalikes() {
    assert!(validate_package_url("https://szxinyu.com/a/app.msi").is_ok());
    assert!(validate_package_url("https://update.szxinyu.com/packages/app.exe").is_ok());
    assert!(validate_package_url("https://evil-szxinyu.com/app.exe").is_err());
    assert!(validate_package_url("http://update.szxinyu.com/app.exe").is_err());
    assert!(validate_package_url("https://update.szxinyu.com:8443/app.exe").is_err());
}

#[test]
fn manifest_requires_sha256_and_matching_installer_extension() {
    let mut manifest = test_manifest("https://update.szxinyu.com/app.msi");
    manifest.sha256 = "".into();
    assert!(validate_manifest(&manifest).is_err());
    manifest.sha256 = "00".repeat(32);
    manifest.installer_type = InstallerType::Exe;
    assert!(validate_manifest(&manifest).is_err());
}

#[test]
fn retry_delay_uses_bounded_backoff() {
    assert_eq!(retry_delay(0), Some(Duration::from_secs(5 * 60)));
    assert_eq!(retry_delay(1), Some(Duration::from_secs(15 * 60)));
    assert_eq!(retry_delay(2), Some(Duration::from_secs(30 * 60)));
    assert_eq!(retry_delay(3), None);
}
```

- [ ] **Step 2: Run the tests to verify the policy is missing**

Run: `cargo test --lib remote_software::tests --features flutter`

Expected: FAIL because the manifest types and validators do not exist yet. If the command cannot run on the development machine, record the unavailable toolchain and defer the same command to Task 6's GitHub Actions verification before final completion.

- [ ] **Step 3: Implement the minimal pure policy module**

Implement the exact interfaces above. Parse with `url::Url`; accept only `https`, no username/password, no IP host, no explicit port except 443, and a hostname equal to `szxinyu.com` or ending in `.szxinyu.com`. Validate the final path extension with `Path::extension`, reject empty names, control characters and non-absolute EXE detection paths, and compare the installer type with the extension. Keep EXE arguments as `Vec<String>` and never join them into a shell command.

Add `OPTION_ALLOW_REMOTE_SOFTWARE_INSTALL = "allow-remote-software-install"` to `libs/hbb_common/src/config.rs`, include it in `KEYS_SETTINGS`, and make `default_option` return `Some("N")` so the policy is off when no custom-client setting enables it.

- [ ] **Step 4: Run the focused tests and the existing Rust unit suite**

Run: `cargo test --lib remote_software::tests --features flutter`

Run: `cargo test --lib config::tests::test_remote_configuration_modification_default_is_enabled --features flutter`

Expected: when a local Rust toolchain is available, both commands exit 0 with no failed tests. If it is unavailable, record both commands as not run and require their successful execution in Task 6's GitHub Actions verification before final completion.

- [ ] **Step 5: Commit the focused unit**

```bash
git add src/lib.rs src/remote_software.rs libs/hbb_common/src/config.rs
git commit -m "feat: add remote software manifest policy"
```

### Task 2: Add wire protocol, capability advertisement and independent permission

**Files:**
- Modify: `libs/hbb_common/protos/rendezvous.proto`
- Modify: `libs/hbb_common/protos/message.proto`
- Modify: `src/server/connection.rs`
- Modify: `src/client.rs`
- Modify: `src/client/io_loop.rs`
- Modify: `src/flutter.rs`
- Test: `src/server/connection.rs` and `src/remote_software.rs` protocol-focused tests

**Interfaces:**
- Adds `control_permissions.Permission.software_install = 13`.
- Adds `Features.software_install = 4`.
- Adds `SoftwareInstallAction` with `request` and `cancel` oneof members.
- Adds `SoftwareInstallStatus` with `request_id`, `stage`, `message`, `exit_code`, `needs_reboot` and `progress_percent`.
- Adds `Message.software_install_action = 34` and `Message.software_install_status = 35`.
- Produces Flutter events named `software_install_status` with `request_id`, `stage`, `message`, `exit_code`, `needs_reboot` and `progress_percent`.

- [ ] **Step 1: Write the failing protocol/capability tests**

```rust
#[test]
fn software_install_request_round_trips_without_shell_command_fields() {
    let request = test_install_request();
    let mut action = SoftwareInstallAction::new();
    action.set_request(request.clone());
    let mut message = Message::new();
    message.set_software_install_action(action);

    let bytes = message.write_to_bytes().unwrap();
    let decoded = Message::parse_from_bytes(&bytes).unwrap();
    let decoded = decoded.software_install_action.unwrap().request.unwrap();
    assert_eq!(decoded.request_id, request.request_id);
    assert!(decoded.silent_args.len() < 16);
}

#[test]
fn software_install_is_not_allowed_on_file_or_terminal_scoped_connections() {
    let message = software_install_message();
    assert_eq!(Connection::message_family(&message), "software_install_action");
    assert!(!Connection::is_file_transfer_scoped_message(&message));
    assert!(!Connection::is_terminal_scoped_message(&message));
}
```

- [ ] **Step 2: Run the protocol tests to verify the message types are missing**

Run: `cargo test --lib software_install_request_round_trips_without_shell_command_fields --features flutter`

Expected: FAIL because the protobuf messages and `Message` union fields are not generated yet.

- [ ] **Step 3: Add the protobuf definitions and regenerate through the normal build**

Add the permission enum value, the `Features` boolean, the request/cancel/action/status messages, and union fields 34 and 35. Keep all existing field numbers unchanged. Use enum values for installer type, detection type, mode and status stage; do not add a command string field.

- [ ] **Step 4: Implement capability and scope handling**

In `src/server/connection.rs`, map `OPTION_ALLOW_REMOTE_SOFTWARE_INSTALL` to `control_permissions::Permission::software_install`, advertise `Features.software_install` only on Windows when the policy is enabled and the RustDesk Windows service is running, and add `SoftwareInstallAction` to the authorized remote message scope only. Reject it for file-transfer, terminal, port-forward and camera scoped connections.

In `src/client/io_loop.rs` and `src/flutter.rs`, parse the capability and forward every status message to Flutter. A legacy peer without the field remains unsupported and never shows the remote-install entry.

- [ ] **Step 5: Run wire and scope tests**

Run: `cargo test --lib software_install --features flutter`

Expected: all focused protocol tests pass and the output contains 0 failures.

- [ ] **Step 6: Commit the protocol unit**

```bash
git add libs/hbb_common/protos/rendezvous.proto libs/hbb_common/protos/message.proto src/server/connection.rs src/client/io_loop.rs src/flutter.rs
git commit -m "feat: add remote software install protocol"
```

### Task 3: Implement the Windows downloader, detector and silent executor

**Files:**
- Create: `src/platform/windows_remote_software.rs`
- Modify: `src/platform/mod.rs`
- Test: `src/platform/windows_remote_software.rs` unit tests

**Interfaces:**
- Produces `pub async fn execute(manifest: RemoteSoftwareManifest, progress: impl Fn(RemoteSoftwareStatus) + Send + Sync)`.
- Produces `pub fn detect_installed(rule: &DetectionRule) -> Result<bool, RemoteSoftwareError>`.
- Produces `pub fn start_self_heal_worker()` for the Windows service process.
- Produces `pub fn service_is_available() -> bool`.
- Uses `RemoteSoftwareManifest`, `RemoteSoftwareStatus` and validation functions from Task 1.

- [ ] **Step 1: Write failing tests for command construction and detection**

```rust
#[cfg(windows)]
#[test]
fn msi_command_uses_msiexec_without_shell() {
    let manifest = test_msi_manifest();
    let command = build_process_command(&manifest, Path::new(r"C:\pkg\office.msi")).unwrap();
    assert_eq!(command.program(), Path::new(r"C:\Windows\System32\msiexec.exe"));
    assert_eq!(command.args(), &["/i", r"C:\pkg\office.msi", "/qn", "/norestart"]);
}

#[cfg(windows)]
#[test]
fn deleted_exe_path_is_reported_as_missing() {
    let path = unique_test_path("missing", "exe");
    assert!(!detect_installed(&DetectionRule::ExePath(path)).unwrap());
}

#[test]
fn installer_exit_codes_map_to_reboot_or_failure() {
    assert_eq!(installer_outcome(Some(0)), InstallOutcome::Success);
    assert_eq!(installer_outcome(Some(3010)), InstallOutcome::NeedsReboot);
    assert_eq!(installer_outcome(Some(1603)), InstallOutcome::Failed);
}
```

- [ ] **Step 2: Run the Windows-focused tests before implementation**

Run on a Windows runner: `cargo test --lib windows_remote_software --features flutter`

Expected: FAIL because the Windows module and command builder do not exist yet.

- [ ] **Step 3: Implement safe download and cache handling**

Use a Reqwest client with redirects disabled. Follow at most five `Location` values manually, validate every location with `validate_package_url`, reject responses above 2 GiB, stream bytes into `<sha256>.part`, compute SHA-256 while writing, flush and close the file, compare the expected digest, then atomically rename it to `<sha256>.<msi|exe>`. Remove the `.part` file on all failures.

Store files below `%ProgramData%\\新育智慧校园\\remote-software\\packages`; create the directory for SYSTEM and local Administrators. Do not log query strings, credentials or full download URLs.

- [ ] **Step 4: Implement fixed detection and shell-free process execution**

For `MsiProductCode`, inspect machine-wide 32-bit and 64-bit uninstall registry views. For `UninstallDisplayName`, require an exact case-insensitive normalized `DisplayName` match in those machine-wide views. For `ExePath`, require an absolute normalized path and check `Path::is_file`.

For MSI, create a `std::process::Command` for the system `msiexec.exe` with `/i`, package path, `/qn` and `/norestart`. For EXE, create a `Command` for the verified package path and append the validated argument vector. Never invoke `cmd.exe`, PowerShell, `ShellExecute`, or a command-line string parser. Use hidden-window creation flags for the child process and wait for its exit code.

- [ ] **Step 5: Add state persistence and the self-heal worker**

Write `state.json` through a temporary file and atomic rename. Save the manifest, cache path, last result, attempt timestamps and failure count. On worker startup, remove stale `.part` files, load valid entries, run one detection pass, and schedule the fixed five-minute interval. When a rule reports missing, call `execute`; on success clear the failure window, and after failures apply the 5/15/30-minute delays with no more than three attempts in one hour. Pause a repeatedly failing entry until the next service restart.

- [ ] **Step 6: Run Windows tests and static checks**

Run: `cargo test --lib windows_remote_software --features flutter`

Run: `cargo fmt --all -- --check`

Expected: exit code 0, no failed tests, and no formatting changes required.

- [ ] **Step 7: Commit the Windows engine**

```bash
git add src/platform/mod.rs src/platform/windows_remote_software.rs
git commit -m "feat: add Windows remote software installer"
```

### Task 4: Connect target authorization, service execution and status reporting

**Files:**
- Modify: `src/server/connection.rs`
- Modify: `src/platform/windows.rs`
- Modify: `src/server.rs`
- Modify: `src/ipc.rs` only if the service-scoped bridge needs a new typed message
- Test: `src/server/connection.rs` and `src/platform/windows_remote_software.rs`

**Interfaces:**
- Consumes `SoftwareInstallAction`, `RemoteSoftwareManifest`, `execute` and `start_self_heal_worker`.
- Produces `installation_denial(policy_enabled: bool, permission_enabled: bool, service_available: bool) -> Option<&'static str>`, returning `permission_denied`, `service_required` or `None`.
- Produces target-side status messages with the same `request_id` for every stage.

- [ ] **Step 1: Write failing authorization and status tests**

```rust
#[test]
fn install_request_is_denied_when_policy_or_service_is_missing() {
    assert_eq!(installation_denial(false, true, true), Some("permission_denied"));
    assert_eq!(installation_denial(true, false, true), Some("permission_denied"));
    assert_eq!(installation_denial(true, true, false), Some("service_required"));
    assert_eq!(installation_denial(true, true, true), None);
}

#[test]
fn status_preserves_request_id_and_terminal_result() {
    let status = RemoteSoftwareStatus::success("request-1", InstallOutcome::NeedsReboot);
    assert_eq!(status.request_id, "request-1");
    assert_eq!(status.needs_reboot, true);
}
```

- [ ] **Step 2: Run the tests before routing exists**

Run: `cargo test --lib installation_denial --features flutter`

Expected: FAIL because the target authorization helper and status mapping are not implemented.

- [ ] **Step 3: Route and authorize requests in `server/connection.rs`**

For `SoftwareInstallAction::request`, first require an authorized remote connection, the independent `software_install` control permission, `Config::get_bool_option(OPTION_ALLOW_REMOTE_SOFTWARE_INSTALL)`, Windows compilation, and `service_is_available()`. Reject all other cases without downloading. Convert the protobuf request to `RemoteSoftwareManifest`, run `validate_manifest`, then call `execute` and send `SoftwareInstallStatus` for queued, downloading, verifying, detecting, installing and terminal states.

For cancel, cancel only the matching active request and remove no installed software. A connection close cancels the in-flight job and leaves only a verified cache and complete state entry.

- [ ] **Step 4: Start self-healing only in the installed Windows service**

Start `start_self_heal_worker()` after the existing Windows service status is registered and before the service enters its IPC loop. Do not start it in a portable user process or ordinary Flutter UI process. Stop the worker when the Windows service receives stop, preshutdown or shutdown.

- [ ] **Step 5: Run integration-focused Rust tests and inspect the diff**

Run: `cargo test --lib installation_denial --features flutter`

Run: `cargo test --lib remote_software --features flutter`

Run: `git diff --check`

Expected: all available tests pass, `git diff --check` exits 0, and no changes appear outside the files listed in this task.

- [ ] **Step 6: Commit target integration**

```bash
git add src/server/connection.rs src/platform/windows.rs src/server.rs src/ipc.rs
git commit -m "feat: authorize and report remote software installs"
```

### Task 5: Add the Flutter remote-install workflow

**Files:**
- Create: `flutter/lib/desktop/widgets/remote_software_install_dialog.dart`
- Create: `flutter/test/remote_software_install_test.dart`
- Modify: `flutter/lib/models/model.dart`
- Modify: `flutter/lib/desktop/pages/remote_page.dart`
- Modify: `flutter/lib/desktop/widgets/remote_toolbar.dart`
- Modify: `src/flutter_ffi.rs`
- Modify: generated bridge files only if the repository build regenerates them

**Interfaces:**
- Produces `RemoteSoftwareInstallForm.validate()` returning field-specific errors.
- Produces `InstallerType::{msi, exe}`, `DetectionType::{msiProductCode, uninstallDisplayName, exePath}` and `InstallMode::{downloadOnly, downloadAndInstall}`.
- Produces `RemoteSoftwareInstallForm { softwareName, packageUrl, sha256, installerType, detectionType, detectionValue, silentArgs, mode }`, whose `validate()` returns `Map<String, String>` field-specific errors.
- Produces `shouldShowRemoteSoftwareInstall(bool targetCapability, bool permissionEnabled) -> bool`.
- Produces `sessionSoftwareInstall(sessionId, manifestJson)` and `sessionSoftwareInstallCancel(sessionId, requestId)` FFI calls.
- Consumes `software_install_status` events and `PeerInfo.features.software_install`.

- [ ] **Step 1: Write the failing Dart form and visibility tests**

```dart
test('accepts an HTTPS szxinyu.com package URL and SHA-256', () {
  final sha256 = List.filled(64, '0').join();
  final form = RemoteSoftwareInstallForm(
    softwareName: '办公软件',
    packageUrl: 'https://update.szxinyu.com/packages/office.msi',
    sha256: sha256,
    installerType: InstallerType.msi,
    detectionType: DetectionType.msiProductCode,
    detectionValue: '{00000000-0000-0000-0000-000000000001}',
  );
  expect(form.validate(), isEmpty);
});

test('rejects a lookalike domain and hides the entry without capability', () {
  final sha256 = List.filled(64, '0').join();
  final form = RemoteSoftwareInstallForm(
    softwareName: '软件',
    packageUrl: 'https://evil-szxinyu.com/app.exe',
    sha256: sha256,
    installerType: InstallerType.exe,
    detectionType: DetectionType.exePath,
    detectionValue: r'C:\Program Files\App\app.exe',
  );
  expect(form.validate(), isNotEmpty);
  expect(shouldShowRemoteSoftwareInstall(false, true), isFalse);
  expect(shouldShowRemoteSoftwareInstall(true, false), isFalse);
});
```

- [ ] **Step 2: Run the Dart tests before implementing the form**

Run: `Set-Location flutter; flutter test test/remote_software_install_test.dart`

Expected: FAIL because the form, enums and visibility helper do not exist.

- [ ] **Step 3: Implement form validation and FFI calls**

Add fields for software name, URL, package type, SHA-256, detection type/value, EXE argument list and mode. Validate the same hostname boundary as Rust before calling FFI, but keep Rust as the authoritative check. Encode only structured JSON matching `RemoteSoftwareManifest`; do not concatenate a command line. Disable buttons while a request is active and show the request ID in debug logs only.

- [ ] **Step 4: Add the remote toolbar entry and status display**

Show “远程安装” only when the target reports Windows software-install capability and the current permission map allows it. Add the dialog to the existing remote toolbar action area. Render queued, downloading, verifying, detecting, installing, already installed, success, needs reboot and failure states from `software_install_status`. Close/cancel must call the cancel FFI method and must not delete the target cache or state record.

- [ ] **Step 5: Run Dart analysis and tests**

Run: `Set-Location flutter; flutter test test/remote_software_install_test.dart`

Run: `Set-Location flutter; flutter analyze lib/models/model.dart lib/desktop/pages/remote_page.dart lib/desktop/widgets/remote_toolbar.dart lib/desktop/widgets/remote_software_install_dialog.dart`

Expected: both commands exit 0 with no analyzer errors and no failed tests.

- [ ] **Step 6: Commit the Flutter workflow**

```bash
git add flutter/lib/models/model.dart flutter/lib/desktop/pages/remote_page.dart flutter/lib/desktop/widgets/remote_toolbar.dart flutter/lib/desktop/widgets/remote_software_install_dialog.dart flutter/test/remote_software_install_test.dart src/flutter_ffi.rs
git commit -m "feat: add remote software install UI"
```

### Task 6: Add CI verification and perform final acceptance

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `.github/workflows/flutter-build.yml` only if the Windows matrix needs an explicit test step
- Test: existing Rust and Flutter test locations from Tasks 1–5

**Interfaces:**
- CI runs the pure policy tests on the normal Rust job.
- CI runs Windows-specific tests and the Windows build on a Windows runner.
- CI runs the new Flutter form tests and analyzer check.

- [ ] **Step 1: Write the CI assertions as a local checklist**

```text
Rust policy/protocol tests: cargo test --lib remote_software --features flutter
Windows installer tests: cargo test --lib windows_remote_software --features flutter
Flutter form tests: flutter test test/remote_software_install_test.dart
Flutter analyzer: flutter analyze lib/models/model.dart lib/desktop/pages/remote_page.dart lib/desktop/widgets/remote_toolbar.dart lib/desktop/widgets/remote_software_install_dialog.dart
Formatting: cargo fmt --all -- --check
Diff whitespace: git diff --check
```

- [ ] **Step 2: Run the checklist on available local toolchains**

Run every command above that is available locally. Record unavailable `cargo` or `flutter` binaries as environment limitations; do not report those tests as passed.

- [ ] **Step 3: Add the same commands to GitHub Actions**

Put Rust and formatting checks in the existing Rust CI job, put the Flutter test/analyzer commands after Flutter setup, and put the Windows installer tests in the Windows job before packaging. Keep the existing `v1.5.1` release workflow unchanged except for the test step needed to gate the build.

- [ ] **Step 4: Verify the complete feature on a Windows runner**

Confirm these cases from logs and test assertions: unauthorized request is rejected before download; non-`szxinyu.com` URL is rejected; valid package downloads and verifies; already-installed software skips execution; MSI/EXE runs without Shell; service restart triggers a missing-software check; deleting the detected EXE causes recovery on the next scheduled cycle; failed installs obey the retry limit; disabling the policy stops new work.

- [ ] **Step 5: Run final verification before claiming completion**

Run: `git status --short --branch`

Run: `git diff HEAD^ --check`

Run: `git log --oneline -8`

Run the GitHub Actions workflow and inspect the final run URL, job conclusions and Windows artifacts. Only report “完成” when the relevant jobs have conclusion `success`; otherwise report the exact failing job and leave the code unclaimed as complete.

- [ ] **Step 6: Commit CI changes**

```bash
git add .github/workflows/ci.yml .github/workflows/flutter-build.yml
git commit -m "test: verify remote software deployment in CI"
```
