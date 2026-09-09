const String kWindowsUninstallPassword = 'xinyu';
const int kWindowsDeviceNameMaxLength = 64;

String homeDeviceNameLabel(String name) {
  final value = name.trim();
  return value.isEmpty ? '未设置' : value;
}

bool isValidWindowsDeviceName(String name) {
  final value = name.trim();
  return value.isNotEmpty && value.length <= kWindowsDeviceNameMaxLength;
}

bool shouldRequestInitialDeviceName({
  required bool isWindows,
  required bool isInstalled,
  required String currentName,
}) {
  return isWindows && isInstalled && currentName.trim().isEmpty;
}

bool isValidWindowsUninstallPassword(String password) {
  return password == kWindowsUninstallPassword;
}
