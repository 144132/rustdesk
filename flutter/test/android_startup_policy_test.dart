import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/mobile/android_startup_policy.dart';

void main() {
  group('Android startup policy', () {
    test('requires a device name when the saved name is empty', () {
      expect(shouldRequestInitialAndroidDeviceName(''), isTrue);
      expect(shouldRequestInitialAndroidDeviceName('   '), isTrue);
    });

    test('does not request a device name when one is already saved', () {
      expect(shouldRequestInitialAndroidDeviceName('办公室手机'), isFalse);
    });

    test('accepts trimmed Android device names up to 64 characters', () {
      expect(isValidAndroidDeviceName('  办公室手机  '), isTrue);
      expect(isValidAndroidDeviceName('a' * 64), isTrue);
      expect(isValidAndroidDeviceName('a' * 65), isFalse);
      expect(isValidAndroidDeviceName('   '), isFalse);
    });

    test('starts the Android service only when it is not ready', () {
      expect(shouldAutoStartAndroidService(serviceReady: false), isTrue);
      expect(shouldAutoStartAndroidService(serviceReady: true), isFalse);
    });
  });
}
