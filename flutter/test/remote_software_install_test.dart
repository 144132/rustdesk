import 'package:flutter_test/flutter_test.dart';
import 'package:rustdesk/desktop/widgets/remote_software_install_dialog.dart';

void main() {
  test('accepts HTTPS szxinyu.com package URL and SHA-256', () {
    final form = RemoteSoftwareInstallForm(
      softwareName: 'RustDesk',
      packageUrl: 'https://szxinyu.com/installer.exe',
      sha256: 'a' * 64,
      detectionValue: 'C:/Program Files/RustDesk/RustDesk.exe',
    );

    expect(form.validate(), isEmpty);
    expect(form.toJson()['packageUrl'], 'https://szxinyu.com/installer.exe');
  });

  test('rejects non-HTTPS and invalid domains', () {
    final form = RemoteSoftwareInstallForm(
      packageUrl: 'http://szxinyu',
      sha256: 'not-a-sha',
      detectionValue: 'RustDesk.exe',
    );

    final errors = form.validate();
    expect(errors['packageUrl'], isNotNull);
    expect(errors['sha256'], isNotNull);
  });

  test('shows install form only with capability and permission', () {
    expect(shouldShowRemoteSoftwareInstall(true, true), isTrue);
    expect(shouldShowRemoteSoftwareInstall(true, false), isFalse);
    expect(shouldShowRemoteSoftwareInstall(false, true), isFalse);
  });
}
