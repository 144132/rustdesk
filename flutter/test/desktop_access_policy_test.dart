import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/consts.dart';
import 'package:flutter_hbb/desktop/desktop_access_policy.dart';

void main() {
  group('Windows settings password', () {
    test('uses the confirmed settings verification password', () {
      expect(kWindowsSettingsPassword, 'wwwyibu');
    });
  });

  group('initial device name prompt', () {
    test('requires a name for a fresh Windows installation', () {
      expect(
        shouldRequestInitialDeviceName(
          isWindows: true,
          isInstalled: true,
          currentName: '',
        ),
        isTrue,
      );
    });

    test('does not prompt when a name already exists', () {
      expect(
        shouldRequestInitialDeviceName(
          isWindows: true,
          isInstalled: true,
          currentName: '办公室电脑',
        ),
        isFalse,
      );
    });

    test('does not prompt for portable or non-Windows clients', () {
      expect(
        shouldRequestInitialDeviceName(
          isWindows: true,
          isInstalled: false,
          currentName: '',
        ),
        isFalse,
      );
      expect(
        shouldRequestInitialDeviceName(
          isWindows: false,
          isInstalled: true,
          currentName: '',
        ),
        isFalse,
      );
    });
  });

  group('Windows uninstall password', () {
    test('accepts the configured password only', () {
      expect(isValidWindowsUninstallPassword('xinyu'), isTrue);
      expect(isValidWindowsUninstallPassword('Xinyu'), isFalse);
      expect(isValidWindowsUninstallPassword(''), isFalse);
    });
  });
}
