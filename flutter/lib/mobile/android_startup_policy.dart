const int kAndroidDeviceNameMaxLength = 64;

bool shouldRequestInitialAndroidDeviceName(String currentName) {
  return currentName.trim().isEmpty;
}

bool isValidAndroidDeviceName(String name) {
  final value = name.trim();
  return value.isNotEmpty && value.length <= kAndroidDeviceNameMaxLength;
}

bool shouldAutoStartAndroidService({required bool serviceReady}) {
  return !serviceReady;
}
