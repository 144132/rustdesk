const String kWindowsUninstallPassword = 'xinyu';

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
