# Windows Default Password Connection-Manager Window Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Windows 被控端使用默认永久密码完成授权的远程桌面连接不自动弹出连接管理器窗口，非默认密码和未授权请求保持现有行为。

**Architecture:** 在 `hbb_common` 配置层提供不泄露明文的有效永久密码匹配 helper；Rust 连接管理器在创建连接状态时计算 `show_cm_window`；Flutter 将该字段带入客户端模型，只在连接要求显示时调用 `showCmWindow()`。认证本身、会话数据通道和 Android 连接弹窗不改变。

**Tech Stack:** Rust、hbb_common password storage、Serde JSON、Flutter/Dart、Provider/ChangeNotifier、Rust 单元测试和 Flutter 测试。

**Spec:** `docs/superpowers/specs/2026-09-19-windows-remote-install-removal-and-password-prompt-design.md`

## Global Constraints

- 本次只调整 Windows 被控端；Android、macOS、Linux 的运行行为不在本次范围内。
- 当 Windows 被控端当前生效的永久密码仍为默认密码 `Ooo000#@!` 时，已授权的远程桌面连接不自动拉起连接管理器窗口。
- 未授权的连接请求必须继续显示连接管理器，以便用户执行接受/拒绝操作。
- 非默认永久密码继续沿用现有提示和接受/拒绝流程。
- 不返回或记录密码明文，仅在内存中比较候选值与存储值对应的哈希。
- 普通远程桌面、文件传输、终端、摄像头、权限同步和远程数据通道不因窗口抑制而改变。
- 每个行为先写失败测试并实际确认失败，再写最小实现；完成前运行完整可用验证命令。

## Review Focus

- 本地永久密码优先于预置密码，且自定义本地密码不能被默认预置密码误判；由 Task 1 的有效密码选择测试覆盖。
- 当前永久密码为空或存储损坏时不能抑制窗口；由 Task 1 的空值/非法存储测试覆盖。
- 默认密码下未授权连接仍需显示确认窗口；由 Task 2 的纯决策测试覆盖。
- 文件传输、终端、摄像头和端口转发不能误用远程桌面抑制规则；由 Task 2 的非远程连接测试覆盖。
- 缺少 `show_cm_window` 的旧 JSON 状态必须继续显示窗口；由 Task 3 的 Flutter 兼容测试覆盖。

---

### Task 1: 增加有效永久密码匹配 helper

**Files:**
- Modify: `libs/hbb_common/src/config/permanent_password.rs`
- Modify: `libs/hbb_common/src/config.rs`
- Test: `libs/hbb_common/src/config/permanent_password.rs` and `libs/hbb_common/src/config.rs` unit tests

**Interfaces:**
- Consumes: Existing local/preset password storage decoders and salt-aware hash helpers.
- Produces: `Config::permanent_password_matches_plain(candidate: &str) -> bool`, with local storage taking precedence over preset storage.

- [ ] **Step 1: Write failing effective-password selector tests**

In the `config.rs` test module, add tests for the pure selector before defining it. Use plain storage in these selector tests so the expected behavior is independent of any new fixture encoder; the existing `permanent_password.rs` tests already cover encrypted/hash comparison:

```rust
const DEFAULT_PASSWORD: &str = "Ooo000#@!";

#[test]
fn effective_permanent_password_matches_plain_uses_local_first() {
    assert!(!effective_permanent_password_matches_plain(
        "custom-password",
        "",
        DEFAULT_PASSWORD,
        "",
        DEFAULT_PASSWORD,
    ));
}

#[test]
fn effective_permanent_password_matches_plain_falls_back_to_preset() {
    assert!(effective_permanent_password_matches_plain(
        "",
        "",
        DEFAULT_PASSWORD,
        "",
        DEFAULT_PASSWORD,
    ));
}

#[test]
fn effective_permanent_password_matches_plain_rejects_invalid_non_empty_local() {
    assert!(!effective_permanent_password_matches_plain(
        "01invalid-storage",
        "salt",
        DEFAULT_PASSWORD,
        "",
        DEFAULT_PASSWORD,
    ));
}
```

- [ ] **Step 2: Run the focused submodule test and verify the intended red failure**

Run from `libs/hbb_common`:

```powershell
cargo test config::tests::effective_permanent_password_matches_plain_uses_local_first
```

Expected: the new tests fail because `effective_permanent_password_matches_plain` does not exist yet, not because of a syntax or fixture error.

- [ ] **Step 3: Add the effective local-over-preset decision helper**

In `libs/hbb_common/src/config/permanent_password.rs`, remove the test-only restriction from `local_permanent_password_storage_matches_plain` and make it `pub(super)` without changing its comparison semantics. In `libs/hbb_common/src/config.rs`, import that helper and add a private pure selector used by the public method:

```rust
fn effective_permanent_password_matches_plain(
    local_storage: &str,
    local_salt: &str,
    preset_storage: &str,
    preset_salt: &str,
    candidate: &str,
) -> bool
```

The selector must use local storage when it is non-empty and otherwise use preset storage. Add:

```rust
pub fn permanent_password_matches_plain(candidate: &str) -> bool
```

This method reads one local snapshot and one preset snapshot, delegates format handling to the existing storage helpers, and never logs or returns a password.

- [ ] **Step 4: Add the public-method regression tests and run them green**

Keep the three selector tests from Step 1 and add these tests using the existing `with_config_and_hard_settings` guard:

```rust
#[test]
fn public_password_match_uses_current_local_password() {
    let mut config = Config::default();
    config.password = "custom-password".to_owned();
    config.salt = String::new();
    let hard_settings = HashMap::from([
        ("password".to_owned(), DEFAULT_PASSWORD.to_owned()),
        ("salt".to_owned(), String::new()),
    ]);

    with_config_and_hard_settings(config, hard_settings, || {
        assert!(!Config::permanent_password_matches_plain(DEFAULT_PASSWORD));
        assert!(Config::permanent_password_matches_plain("custom-password"));
    });
}

#[test]
fn public_password_match_falls_back_to_preset_when_local_is_empty() {
    let config = Config::default();
    let hard_settings = HashMap::from([
        ("password".to_owned(), DEFAULT_PASSWORD.to_owned()),
        ("salt".to_owned(), String::new()),
    ]);

    with_config_and_hard_settings(config, hard_settings, || {
        assert!(Config::permanent_password_matches_plain(DEFAULT_PASSWORD));
    });
}
```

Run:

```powershell
cargo test config::
cargo fmt --check
```

Expected: all focused tests pass and formatting is clean.

- [ ] **Step 5: Commit the helper independently**

Commit the child submodule changes only:

```powershell
git -C libs/hbb_common add -- src/config.rs src/config/permanent_password.rs
git -C libs/hbb_common commit -m "feat: expose effective permanent password check"
```

Update the parent gitlink in a later task after the Rust/UI behavior is complete.

### Task 2: Add tested Rust connection-manager display policy

**Files:**
- Modify: `src/ui_cm_interface.rs`
- Test: `src/ui_cm_interface.rs` unit tests

**Interfaces:**
- Consumes: `Config::permanent_password_matches_plain`, `crate::common::DEFAULT_PERMANENT_PASSWORD`, existing `ConnectionManager::add_connection` arguments.
- Produces: `Client.show_cm_window: bool` and `should_show_cm_window(is_windows: bool, authorized: bool, is_remote_desktop: bool, permanent_password_is_default: bool) -> bool`.

- [ ] **Step 1: Write failing policy tests**

Add a pure function test module before implementing the function:

```rust
#[test]
fn authorized_default_password_remote_desktop_does_not_show_cm() {
    assert!(!should_show_cm_window(true, true, true, true));
}

#[test]
fn custom_password_keeps_cm_visible() {
    assert!(should_show_cm_window(true, true, true, false));
}

#[test]
fn unauthorized_default_password_still_shows_cm_for_acceptance() {
    assert!(should_show_cm_window(true, false, true, true));
}

#[test]
fn non_remote_connections_keep_existing_cm_behavior() {
    assert!(should_show_cm_window(true, true, false, true));
}
```

- [ ] **Step 2: Run the focused Rust test and verify it fails for the missing policy**

Run:

```powershell
cargo test ui_cm_interface::tests::authorized_default_password_remote_desktop_does_not_show_cm
```

Expected: compile/test failure because the pure policy function does not exist yet.

- [ ] **Step 3: Implement the minimal policy and serialize the per-connection flag**

Add `should_show_cm_window` near the `Client`/`ConnectionManager` code. The function returns `false` only when all four conditions are true: Windows, authorized, remote desktop, and default effective permanent password. Otherwise it returns `true`.

Add `show_cm_window: bool` to the serialized Rust `Client`. In `ConnectionManager::add_connection`, compute `is_remote_desktop` before moving `port_forward` into `Client`:

```rust
let is_remote_desktop = !is_file_transfer
    && !is_view_camera
    && !is_terminal
    && port_forward.is_empty();
let permanent_password_is_default = cfg!(target_os = "windows")
    && Config::permanent_password_matches_plain(crate::common::DEFAULT_PERMANENT_PASSWORD);
let show_cm_window = should_show_cm_window(
    cfg!(target_os = "windows"),
    authorized,
    is_remote_desktop,
    permanent_password_is_default,
);
```

Store the result in the `Client`; do not alter `authorized`, send a new accept message, or change any connection authentication branch.

- [ ] **Step 4: Run the policy tests green and inspect the diff**

Run:

```powershell
cargo test ui_cm_interface::tests
cargo fmt --check
```

Expected: all four policy cases pass; the only production change is the display metadata and its calculation.

### Task 3: Consume the display flag in Flutter without changing Android behavior

**Files:**
- Modify: `flutter/lib/models/server_model.dart`
- Test: `flutter/test/server_model_window_test.dart`

**Interfaces:**
- Consumes: Rust JSON field `show_cm_window` and existing `showCmWindow()` / `hideCmWindow()` functions.
- Produces: `Client.showCmWindow: bool`, legacy JSON fallback `true`, and a tested Flutter display predicate.

- [ ] **Step 1: Write failing Flutter model/policy tests**

Create `flutter/test/server_model_window_test.dart` with a pure predicate and legacy JSON cases:

```dart
test('suppressed authorized Windows remote does not request CM window', () {
  expect(shouldShowConnectionManagerWindow(true, false, false), isFalse);
});

test('custom password connection keeps requesting CM window', () {
  expect(shouldShowConnectionManagerWindow(true, false, true), isTrue);
});

test('missing display flag keeps legacy visible behavior', () {
  final client = Client.fromJson(legacyClientJson());
  expect(client.showCmWindow, isTrue);
});
```

`legacyClientJson()` must include all fields currently required by `Client.fromJson`, with no `show_cm_window` key.

Use this complete legacy fixture:

```dart
Map<String, dynamic> legacyClientJson() => {
      'id': 1,
      'authorized': true,
      'is_file_transfer': false,
      'is_view_camera': false,
      'is_terminal': false,
      'port_forward': '',
      'name': 'peer',
      'avatar': '',
      'peer_id': 'peer-id',
      'keyboard': true,
      'clipboard': true,
      'audio': true,
      'file': true,
      'restart': false,
      'recording': false,
      'block_input': false,
      'privacy_mode': false,
      'disconnected': false,
      'from_switch': false,
      'in_voice_call': false,
      'incoming_voice_call': false,
    };
```

- [ ] **Step 2: Run the focused Flutter test and verify the expected red failure**

Run from `flutter`:

```powershell
flutter test test/server_model_window_test.dart
```

Expected: failure because `showCmWindow` and `shouldShowConnectionManagerWindow` do not yet exist.

- [ ] **Step 3: Add model decoding and display gating**

Add `bool showCmWindow = true` to `Client`. Decode with `json['show_cm_window'] ?? true` and include it in `toJson()`.

Add the pure predicate:

```dart
bool shouldShowConnectionManagerWindow(
    bool isConnectionManager, bool hideCm, bool showCmWindow) {
  return isConnectionManager && !hideCm && showCmWindow;
}
```

In `updateClientState`, call `showCmWindow()` only when at least one loaded client has `showCmWindow == true`; otherwise keep the connection manager hidden. In `addConnection`, update the existing client’s `showCmWindow` when an unauthorized connection transitions to authorized, and call `showCmWindow()` only when the incoming client’s flag is true. Keep the Android-only `showLoginDialog` branch unchanged.

- [ ] **Step 4: Run Flutter tests/analyzer green**

Run:

```powershell
flutter test test/server_model_window_test.dart
flutter analyze lib/models/server_model.dart
```

Expected: focused tests pass, legacy JSON defaults to visible, and analyzer reports no errors.

- [ ] **Step 5: Commit the display behavior**

Run:

```powershell
git add -- src/ui_cm_interface.rs flutter/lib/models/server_model.dart flutter/test/server_model_window_test.dart
git commit -m "fix: suppress Windows CM window for default password"
```

### Task 4: Integrate, verify regression boundaries and update the parent submodule pointer

**Files:**
- Modify: Parent `libs/hbb_common` gitlink
- Test: Full Rust and Flutter suites, residual scans and final diff review

**Interfaces:**
- Consumes: The password helper, Rust display metadata and Flutter model behavior from Tasks 1–3.
- Produces: Verified Windows-only window suppression with no Android behavior change.

- [ ] **Step 1: Verify the submodule child diff before updating the parent**

Run:

```powershell
git -C libs/hbb_common status --short --branch
git -C libs/hbb_common show --stat --oneline HEAD
git diff --submodule=diff -- libs/hbb_common
```

Expected: only the effective-password helper and its tests are present for this plan; no remote-install schema or unrelated Android change is removed.

- [ ] **Step 2: Run the full available verification commands**

Run from the parent:

```powershell
cargo fmt --check
cargo test --lib --features flutter
cargo check --locked --features flutter
```

Run from `flutter`:

```powershell
flutter analyze lib/models/server_model.dart
flutter test
```

Expected: exit code 0 for every available command. If a toolchain is unavailable, record the exact unverified command and do not claim a passing build.

- [ ] **Step 3: Verify the final runtime behavior surface**

Run:

```powershell
rg -n -S "show_cm_window|showCmWindow|shouldShowConnectionManagerWindow|permanent_password_matches_plain" src flutter libs/hbb_common
git diff --check
git status --short
```

Expected: the new symbols occur only in the password helper, CM state bridge, Flutter model/test and their necessary call sites. Existing unrelated files remain untouched.

- [ ] **Step 4: Update the parent gitlink and commit the integration**

After the child helper commit is verified, stage only the parent gitlink and commit:

```powershell
git add -- libs/hbb_common
git commit -m "chore: update password helper submodule"
```

- [ ] **Step 5: Perform the final regression-surface review**

Inspect every modified existing file. Confirm explicitly:

1. `src/ui_cm_interface.rs` only adds serialized display metadata and does not alter authorization.
2. `flutter/lib/models/server_model.dart` only gates automatic window display and keeps Android dialog behavior unchanged.
3. `hbb_common` only reads password storage and does not expose plaintext.
4. Existing connection manager, file transfer, terminal, camera and port-forward paths retain their previous display behavior.
