# Windows Remote Software Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 从 Windows RustDesk 客户端移除远程静默安装软件的完整运行时、协议、权限、UI、测试和 CI 链路，同时保留普通远程控制和其他现有功能。

**Architecture:** 以功能边界定向删除远程安装专属代码和 schema，不使用大范围 `git revert`。先拆除 Flutter/控制端调用，再拆除被控端执行器和会话处理，最后清理 `hbb_common` 子模块协议与策略配置，并用全局残留扫描和完整测试确认普通连接链路仍然闭合。

**Tech Stack:** Rust、Tokio、RustDesk protobuf、Flutter/Dart、GitHub Actions、Windows 服务代码。

**Spec:** `docs/superpowers/specs/2026-09-19-windows-remote-install-removal-and-password-prompt-design.md`

## Global Constraints

- 本次只调整 Windows 被控端；Android、macOS、Linux 的运行行为不在本次范围内。
- 移除时采用按功能边界的定向删除，不对当前分支做大范围 `git revert`。
- `hbb_common` 子模块必须保留与本需求无关的 Windows/Android 默认永久密码逻辑。
- 工作区已有的脚本、APK 工作流、设备分组、版本显示和其他无关未提交文件不修改。
- 普通远程桌面、文件传输、终端、摄像头、权限同步和现有 Windows 服务功能继续沿用原路径。
- 历史设计文档保留为开发记录，不参与构建和运行时引用。
- 每个行为先写失败测试并实际确认失败，再写最小实现；完成前运行完整可用验证命令，不把未运行报告为通过。

## Review Focus

- 当前工作区未提交的 `SoftwareInstall` 权限同步和 protobuf 枚举必须随功能移除，不得误留一个可用的后门入口；由 Task 1 和 Task 3 的残留扫描覆盖。
- 删除 Windows 自愈 worker 时不能删除普通 RustDesk Windows 服务启动、更新或卸载逻辑；由 Task 2 的编译和服务引用扫描覆盖。
- `hbb_common` 子模块同时包含默认密码相关改动，不能回退整个子模块；由 Task 3 的子模块差异检查覆盖。
- 旧远程端发送的软件安装 protobuf 字段不能让新客户端重新暴露安装功能；由 Task 3 的 schema 生成与普通消息测试覆盖。
- CI 中只删除远程安装专用测试/构建参数，不能删掉普通 Rust、Flutter 或 Windows 构建 job；由 Task 4 的 workflow 扫描覆盖。

---

### Task 1: 删除控制端能力、Flutter 入口和 FFI

**Files:**
- Modify: `src/client.rs`
- Modify: `src/client/io_loop.rs`
- Modify: `src/flutter.rs`
- Modify: `src/flutter_ffi.rs`
- Modify: `src/ui_session_interface.rs`
- Modify: `src/lib.rs`
- Modify: `src/platform/mod.rs`
- Modify: `flutter/lib/models/model.dart`
- Modify: `flutter/lib/desktop/pages/remote_page.dart`
- Modify: `flutter/lib/desktop/widgets/remote_toolbar.dart`
- Delete: `flutter/lib/desktop/widgets/remote_software_install_dialog.dart`
- Delete: `flutter/test/remote_software_install_test.dart`
- Test: Rust compile checks and Flutter analyzer/test suite after the complete removal

**Interfaces:**
- Consumes: Existing `Remote`/`Session` permission and Flutter event paths.
- Produces: No `software_install` capability, permission, FFI export, toolbar item, model state or event handler.

- [ ] **Step 1: Capture the exact control-side inventory before editing**

Run:

```powershell
rg -n -S --hidden --glob '!target/**' --glob '!build/**' `
  "software_install|remote_software|SoftwareInstall|allow-remote-software-install" `
  src flutter .github Cargo.toml Cargo.lock
```

Expected: every match is classified as one of the files listed in this task or as an unrelated historical document excluded from runtime edits. Do not use `git add -A` later because the working tree already contains unrelated files.

- [ ] **Step 2: Remove Rust control-side capability and event plumbing**

Delete the `software_install_feature_supported` and `software_install_stage_name` helpers from `src/client.rs`. Remove the `support_software_install` peer state, `Permission::SoftwareInstall` mapping, permission update branch and `SoftwareInstallStatus` event branch from `src/client/io_loop.rs`.

Remove the advertised Flutter feature and status callback from `src/flutter.rs`. Remove the request parser, result helper, tests and `session_software_install` / `session_software_install_cancel` exports from `src/flutter_ffi.rs`. Remove the session-interface status callback that only forwards installation state. Remove `pub mod remote_software;` and the Windows remote software module declaration from `src/lib.rs` and `src/platform/mod.rs` only after all their consumers are gone.

- [ ] **Step 3: Remove Flutter state, UI entry and feature-only tests**

In `flutter/lib/models/model.dart`, remove the software-install stage/status types, request state, status dispatch, FFI calls and feature decoding. In `flutter/lib/desktop/widgets/remote_toolbar.dart`, remove the import, toolbar item, install form submission and status dialog. In `flutter/lib/desktop/pages/remote_page.dart`, remove only parameters and gates that exist solely for the remote-install toolbar item; preserve all other toolbar construction and Windows gates.

Delete `remote_software_install_dialog.dart` and `remote_software_install_test.dart`. Do not remove ordinary remote toolbar actions or generic permission handling.

- [ ] **Step 4: Remove control-side CI references**

Remove only the Flutter test/analyze paths and Rust `--lib remote_software` / `--lib windows_remote_software` arguments that exist for this feature from `.github/workflows/ci.yml` and `.github/workflows/flutter-build.yml`. Keep the surrounding Rust, Flutter and Windows build jobs intact.

- [ ] **Step 5: Verify the control-side residual scan and commit the bounded deletion**

Run:

```powershell
rg -n -S --hidden --glob '!target/**' --glob '!build/**' `
  "software_install|remote_software|SoftwareInstall|allow-remote-software-install" `
  src/client.rs src/client/io_loop.rs src/flutter.rs src/flutter_ffi.rs `
  src/ui_session_interface.rs src/lib.rs src/platform/mod.rs `
  flutter/lib flutter/test .github/workflows
```

Expected: no control-side runtime, UI, test or workflow matches. Run `git diff --check` and commit only the explicitly modified/deleted files from this task:

```powershell
git add -- src/client.rs src/client/io_loop.rs src/flutter.rs src/flutter_ffi.rs `
  src/ui_session_interface.rs src/lib.rs src/platform/mod.rs `
  flutter/lib/models/model.dart flutter/lib/desktop/pages/remote_page.dart `
  flutter/lib/desktop/widgets/remote_toolbar.dart `
  flutter/lib/desktop/widgets/remote_software_install_dialog.dart `
  flutter/test/remote_software_install_test.dart `
  .github/workflows/ci.yml .github/workflows/flutter-build.yml
git commit -m "refactor: remove remote software install client surface"
```

Expected: the commit contains no unrelated workflow, script or source changes.

### Task 2: Delete host-side remote installer, self-healing and connection protocol handling

**Files:**
- Modify: `src/server/connection.rs`
- Modify: `src/platform/windows.rs`
- Delete: `src/platform/windows_remote_software.rs`
- Delete: `src/remote_software.rs`
- Test: Existing Rust connection, Windows service and ordinary message-scope tests

**Interfaces:**
- Consumes: Existing authorized connection, Windows service and ordinary protobuf message handling.
- Produces: No installer task state, installation event channel, request/cancel handler, capability advertisement or remote-install message family.

- [ ] **Step 1: Remove installer state and event-loop plumbing from `Connection`**

In `src/server/connection.rs`, remove the remote software imports, `SoftwareInstallEvent`, `ActiveSoftwareInstall`, request-id set, active-task field, installation event channel, `tokio::select!` branch and shutdown cancellation hook. Remove the software-install permission advertisement and any special authorized-scope exception that exists only to route installation actions.

- [ ] **Step 2: Remove request/status handlers and helper tests**

Delete the installation status conversion, failure response, request validation/denial, cancellation, task launch, state-store reservation and message-family branches. Remove only tests whose subject is remote software installation. Preserve ordinary authorized message scope validation and all non-installation connection paths.

- [ ] **Step 3: Remove Windows service integration without touching ordinary service behavior**

Delete `src/platform/windows_remote_software.rs`. In `src/platform/windows.rs`, remove only worker startup, state-store and remote-install service hooks. Keep the existing RustDesk service installation, startup, update, tray and shutdown code unchanged.

- [ ] **Step 4: Compile the host-side removal before schema cleanup**

Run:

```powershell
cargo fmt --check
cargo test --lib --features flutter server::connection::test
```

Expected at this intermediate point: any failures identify remaining schema or policy references, not missing installer implementation symbols. Record unavailable-toolchain failures instead of claiming a pass.

- [ ] **Step 5: Commit the host-side deletion**

Run `git diff --check`, inspect every modified hunk in `src/server/connection.rs` and `src/platform/windows.rs`, then commit only the two modified files and the two deleted modules:

```powershell
git add -- src/server/connection.rs src/platform/windows.rs `
  src/platform/windows_remote_software.rs src/remote_software.rs
git commit -m "refactor: remove Windows remote software installer"
```

### Task 3: Remove protobuf permission/capability/schema and submodule policy

**Files:**
- Modify: `libs/hbb_common/protos/message.proto`
- Modify: `libs/hbb_common/protos/rendezvous.proto`
- Modify: `libs/hbb_common/src/config.rs`
- Modify: `libs/hbb_common/src/config/permanent_password.rs` only if required to preserve the current default-password change while removing feature policy code
- Modify: Parent `libs/hbb_common` gitlink in `rustdesk-144132`
- Test: `hbb_common` protobuf/config tests and parent Rust compile

**Interfaces:**
- Consumes: Current submodule checkout at `1d5bf5f` plus its working-tree `SoftwareInstall` permission addition.
- Produces: The same non-feature `hbb_common` behavior, without remote-install permission, feature, action/status messages or policy option.

- [ ] **Step 1: Inspect the submodule commit range and isolate feature changes**

Run:

```powershell
git -C libs/hbb_common log --oneline --decorate -8
git -C libs/hbb_common diff -- protos/message.proto
git -C libs/hbb_common show --stat ddbfbab
git -C libs/hbb_common show --stat 1a53c5a
git -C libs/hbb_common show --stat 1d5bf5f
```

Expected: remote-install protocol/policy changes are separated from the later Android/default-password lock. Do not reset the submodule or discard its working tree wholesale.

- [ ] **Step 2: Remove the feature fields and policy while preserving default-password behavior**

Remove `SoftwareInstall` from the control-permission enum, software-install action/status/message fields and rendezvous capability. Remove `OPTION_ALLOW_REMOTE_SOFTWARE_INSTALL` and only its feature policy/default entries from `config.rs`. Preserve the `1d5bf5f` Android permanent-password lock and all unrelated config/proto changes.

Regenerate any checked-in protobuf output only through the repository’s existing generation mechanism; do not hand-edit generated files beyond the project’s established workflow.

- [ ] **Step 3: Run submodule tests and create a focused child commit**

Run from `libs/hbb_common`:

```powershell
cargo fmt --check
cargo test config::permanent_password
```

Expected: password/config tests pass and no test references the removed feature. Commit only the feature-removal changes in the submodule:

```powershell
git -C libs/hbb_common add -- protos/message.proto protos/rendezvous.proto src/config.rs src/config/permanent_password.rs
git -C libs/hbb_common commit -m "refactor: remove remote software install protocol"
```

- [ ] **Step 4: Update and inspect the parent gitlink**

Return to the parent repository, inspect `git diff --submodule=diff -- libs/hbb_common`, and ensure the child commit range contains no unrelated Android/default-password deletion. Stage only the gitlink:

```powershell
git add -- libs/hbb_common
git commit -m "refactor: remove remote software install schema"
```

### Task 4: Remove feature-only dependencies and perform full residual verification

**Files:**
- Modify: `Cargo.toml` and `Cargo.lock` only if a dependency became feature-only
- Modify: `.github/workflows/ci.yml` and `.github/workflows/flutter-build.yml` only for remaining feature references
- Test: Full Rust/Flutter verification and residual scans

**Interfaces:**
- Consumes: Cleaned Rust, Flutter and `hbb_common` trees from Tasks 1–3.
- Produces: A buildable client with no active remote software installation capability.

- [ ] **Step 1: Re-scan all runtime files and dependency manifests**

Run:

```powershell
rg -n -S --hidden --glob '!target/**' --glob '!build/**' `
  "software_install|remote_software|SoftwareInstall|allow-remote-software-install" `
  src flutter libs/hbb_common Cargo.toml Cargo.lock .github/workflows
```

Expected: no matches in runtime source, tests, manifests or workflows. Matches may remain only in the retained historical `docs/superpowers/specs/` and `docs/superpowers/plans/` records.

- [ ] **Step 2: Remove only now-unused feature dependencies**

Use `cargo check --locked --features flutter` or the available repository dependency inspection to identify dependencies that became unused solely because the installer was deleted. Remove only those entries, regenerate `Cargo.lock` with the repository’s locked workflow, and do not update unrelated dependency versions.

- [ ] **Step 3: Run Rust formatting, tests and compile checks**

Run:

```powershell
cargo fmt --check
cargo test --lib --features flutter
cargo check --locked --features flutter
```

Expected: exit code 0 for each available command. If `cargo` is unavailable or dependency resolution is blocked, report the exact command and failure as unverified.

- [ ] **Step 4: Run Flutter analysis and tests**

Run from `flutter`:

```powershell
flutter analyze lib/models/model.dart lib/desktop/pages/remote_page.dart lib/desktop/widgets/remote_toolbar.dart
flutter test
```

Expected: no analyzer errors, no missing generated binding for the removed FFI exports, and no failed tests.

- [ ] **Step 5: Verify the final diff and commit the cleanup**

Run:

```powershell
git diff --check
git status --short
git diff --stat 9bfecbf8..HEAD
```

Inspect that all commits in this plan contain only feature removal or feature-specific dependency cleanup. Do not stage `.github/workflows/junnuo3576-apk.yml`, `scripts/`, the existing unrelated `src` modifications, or any unrelated submodule changes.
