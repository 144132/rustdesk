import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/desktop/widgets/remote_software_install_dialog.dart';

RemoteSoftwareInstallForm _validForm({
  String? requestId,
  InstallerType installerType = InstallerType.exe,
  DetectionType detectionType = DetectionType.exePath,
  String? packageUrl,
  String? detectionValue,
  List<String>? silentArgs,
}) {
  return RemoteSoftwareInstallForm(
    requestId: requestId,
    softwareName: 'RustDesk',
    packageUrl: packageUrl ?? 'https://szxinyu.com/download/RustDesk.exe',
    sha256: 'a' * 64,
    installerType: installerType,
    detectionType: detectionType,
    detectionValue: detectionValue ?? r'C:\Program Files\RustDesk\RustDesk.exe',
    silentArgs: silentArgs ?? <String>['/S'],
    mode: InstallMode.downloadAndInstall,
  );
}

void main() {
  test('accepts the szxinyu.com HTTPS package and a 64-hex SHA-256', () {
    final form = _validForm();

    expect(form.validate(), isEmpty);
  });

  test('accepts a real szxinyu.com subdomain but rejects a lookalike domain', () {
    expect(
      _validForm(packageUrl: 'https://update.szxinyu.com/RustDesk.exe').validate(),
      isEmpty,
    );
    expect(
      _validForm(packageUrl: 'https://evil-szxinyu.com/RustDesk.exe').validate(),
      contains('packageUrl'),
    );
  });

  test('rejects non-HTTPS URLs and installer extension mismatches', () {
    expect(
      _validForm(packageUrl: 'http://szxinyu.com/RustDesk.exe').validate(),
      contains('packageUrl'),
    );
    expect(
      _validForm(
        installerType: InstallerType.msi,
        detectionType: DetectionType.msiProductCode,
        packageUrl: 'https://szxinyu.com/RustDesk.exe',
        detectionValue: '{12345678-1234-1234-1234-1234567890AB}',
        silentArgs: <String>[],
      ).validate(),
      contains('packageUrl'),
    );
  });

  test('matches Rust port and final filename rules', () {
    expect(
      _validForm(packageUrl: 'https://szxinyu.com:443/RustDesk.exe').validate(),
      isEmpty,
    );
    expect(
      _validForm(packageUrl: 'https://szxinyu.com:8443/RustDesk.exe').validate(),
      contains('packageUrl'),
    );
    expect(
      _validForm(packageUrl: 'https://szxinyu.com/.exe').validate(),
      contains('packageUrl'),
    );
    expect(
      _validForm(packageUrl: 'https://szxinyu.com/').validate(),
      contains('packageUrl'),
    );
  });

  test('rejects invalid SHA-256 and control characters by field', () {
    final errors = RemoteSoftwareInstallForm(
      sha256: 'g' * 64,
      softwareName: 'RustDesk\nAgent',
      packageUrl: 'https://szxinyu.com/RustDesk.exe',
      detectionValue: r'C:\Program Files\RustDesk\RustDesk.exe',
      silentArgs: <String>['/S\u0000'],
    ).validate();

    expect(errors, contains('sha256'));
    expect(errors, contains('softwareName'));
    expect(errors, contains('silentArgs'));
  });

  test('validates MSI product codes and Windows absolute exe paths', () {
    expect(
      _validForm(
        installerType: InstallerType.msi,
        detectionType: DetectionType.msiProductCode,
        packageUrl: 'https://szxinyu.com/RustDesk.msi',
        detectionValue: '{12345678-1234-1234-1234-1234567890AB}',
        silentArgs: <String>[],
      ).validate(),
      isEmpty,
    );
    expect(
      _validForm(
        detectionValue: r'C:relative\RustDesk.exe',
      ).validate(),
      contains('detectionValue'),
    );
    expect(
      _validForm(
        installerType: InstallerType.msi,
        detectionType: DetectionType.msiProductCode,
        packageUrl: 'https://szxinyu.com/RustDesk.msi',
        detectionValue: 'not-a-product-code',
        silentArgs: <String>[],
      ).validate(),
      contains('detectionValue'),
    );
    expect(
      _validForm(
        detectionType: DetectionType.uninstallDisplayName,
        detectionValue: 'RustDesk',
      ).validate(),
      isEmpty,
    );
  });

  test('does not allow silent arguments for MSI installers', () {
    final errors = _validForm(
      installerType: InstallerType.msi,
      detectionType: DetectionType.msiProductCode,
      packageUrl: 'https://szxinyu.com/RustDesk.msi',
      detectionValue: '{12345678-1234-1234-1234-1234567890AB}',
      silentArgs: <String>['/quiet'],
    ).validate();

    expect(errors, contains('silentArgs'));
  });

  test('serializes the structured snake_case manifest and optional request id', () {
    final form = _validForm(requestId: 'request-1');

    expect(form.toJson(), <String, dynamic>{
      'request_id': 'request-1',
      'software_name': 'RustDesk',
      'package_url': 'https://szxinyu.com/download/RustDesk.exe',
      'sha256': 'a' * 64,
      'installer_type': 'exe',
      'detection_rule': <String, String>{
        'exe_path': r'C:\Program Files\RustDesk\RustDesk.exe',
      },
      'silent_args': <String>['/S'],
      'mode': 'download_and_install',
    });
    expect(form.toJson(requestId: 'request-2')['request_id'], 'request-2');
    expect(
      RemoteSoftwareInstallForm().toJson()['request_id'],
      matches(RegExp(r'^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$')),
    );
    expect(form.toJson().keys, isNot(contains('command')));
    expect(form.toJson().keys, isNot(contains('shell')));
    expect(form.toJson().keys, isNot(contains('script')));
    expect(form.toJson().keys, isNot(contains('detection_type')));
    expect(form.toJson().keys, isNot(contains('detection_value')));
  });

  test('exposes the agreed enum values and gates visibility by both inputs', () {
    expect(InstallerType.values, <InstallerType>[InstallerType.msi, InstallerType.exe]);
    expect(DetectionType.values, <DetectionType>[
      DetectionType.msiProductCode,
      DetectionType.uninstallDisplayName,
      DetectionType.exePath,
    ]);
    expect(InstallMode.values, <InstallMode>[
      InstallMode.downloadOnly,
      InstallMode.downloadAndInstall,
    ]);
    expect(shouldShowRemoteSoftwareInstall(true, true), isTrue);
    expect(shouldShowRemoteSoftwareInstall(true, false), isFalse);
    expect(shouldShowRemoteSoftwareInstall(false, true), isFalse);
  });

  testWidgets('renders inputs and invokes submit and cancel callbacks', (tester) async {
    RemoteSoftwareInstallForm? submitted;
    var cancelled = false;

    await tester.pumpWidget(
      MaterialApp(
        home: RemoteSoftwareInstallDialog(
          initialForm: _validForm(),
          onSubmit: (form) => submitted = form,
          onCancel: () => cancelled = true,
        ),
      ),
    );

    expect(find.byType(TextField), findsAtLeastNWidgets(5));
    await tester.tap(find.text('提交'));
    expect(submitted, isNotNull);
    await tester.tap(find.text('取消'));
    expect(cancelled, isTrue);
  });
}
