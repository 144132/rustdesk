import 'dart:async';

import 'package:flutter/material.dart';

import '../../common.dart';
import '../../consts.dart';
import 'android_startup_policy.dart';

Future<String?> showAndroidDeviceNameDialog(
  BuildContext context, {
  String initialName = '',
  bool required = false,
}) {
  final controller = TextEditingController(text: initialName);
  final future = showDialog<String>(
    context: context,
    barrierDismissible: !required,
    builder: (dialogContext) => StatefulBuilder(
      builder: (context, setState) {
        final valid = isValidAndroidDeviceName(controller.text);
        return AlertDialog(
          title: Text(required ? '设置设备名称' : '修改设备名称'),
          content: TextField(
            controller: controller,
            autofocus: true,
            maxLength: kAndroidDeviceNameMaxLength,
            textInputAction: TextInputAction.done,
            decoration: const InputDecoration(
              labelText: '设备名称',
              hintText: '请输入设备名称',
            ),
            onChanged: (_) => setState(() {}),
          ),
          actions: [
            if (!required)
              TextButton(
                onPressed: () => Navigator.of(dialogContext).pop(),
                child: const Text('取消'),
              ),
            FilledButton(
              onPressed: valid
                  ? () => Navigator.of(dialogContext)
                      .pop(controller.text.trim())
                  : null,
              child: const Text('保存'),
            ),
          ],
        );
      },
    ),
  );
  return future.whenComplete(controller.dispose);
}

Future<String?> editAndroidDeviceName(BuildContext context) async {
  final currentName =
      bind.mainGetOptionSync(key: kOptionPresetDeviceName).trim();
  final value = await showAndroidDeviceNameDialog(
    context,
    initialName: currentName,
  );
  if (value != null) {
    await bind.mainSetOption(key: kOptionPresetDeviceName, value: value);
  }
  return value;
}

Future<void> runAndroidStartupFlow(BuildContext context) async {
  if (!isAndroid || bind.isOutgoingOnly()) {
    return;
  }

  final currentName =
      bind.mainGetOptionSync(key: kOptionPresetDeviceName).trim();
  if (shouldRequestInitialAndroidDeviceName(currentName)) {
    final value = await showAndroidDeviceNameDialog(
      context,
      required: true,
    );
    if (value == null || !context.mounted) {
      return;
    }
    await bind.mainSetOption(key: kOptionPresetDeviceName, value: value);
  }

  // Keep the Android boot receiver enabled for unattended startup.
  await gFFI.invokeMethod(AndroidChannel.kSetStartOnBootOpt, true);

  // A boot receiver may already have started the service before Flutter opens.
  final serviceReady = await _isAndroidServiceReady();
  if (!shouldAutoStartAndroidService(
      serviceReady: serviceReady)) {
    return;
  }

  await _prepareAndroidStartupPermissions();

  // Accessibility is protected by Android and requires the user to enable
  // this app in system settings. Continue automatically when the user returns.
  if (!await AndroidPermissionManager.checkInput()) {
    AndroidPermissionManager.startAction(kActionAccessibilitySettings);
    await _waitForInputPermission();
  }

  if (context.mounted && !gFFI.serverModel.isStart) {
    await gFFI.serverModel.startService();
  }
}

Future<void> _prepareAndroidStartupPermissions() async {
  if (androidVersion >= 33 &&
      !await AndroidPermissionManager.check(kAndroid13Notification)) {
    await AndroidPermissionManager.request(kAndroid13Notification);
  }

  if (androidVersion >= 23 &&
      !await AndroidPermissionManager.check(
          kRequestIgnoreBatteryOptimizations)) {
    await AndroidPermissionManager.request(kRequestIgnoreBatteryOptimizations);
  }

  if (bind.mainGetLocalOption(key: kOptionDisableFloatingWindow) != 'Y') {
    await gFFI.serverModel.checkFloatingWindowPermission();
  }
}

Future<bool> _waitForInputPermission() async {
  for (var i = 0; i < 120; i++) {
    if (await AndroidPermissionManager.checkInput()) {
      return true;
    }
    await Future<void>.delayed(const Duration(seconds: 1));
  }
  return false;
}

Future<bool> _isAndroidServiceReady() async {
  for (var i = 0; i < 10; i++) {
    await gFFI.invokeMethod('check_service');
    if (gFFI.serverModel.isStart) {
      return true;
    }
    await Future<void>.delayed(const Duration(milliseconds: 150));
  }
  return false;
}
