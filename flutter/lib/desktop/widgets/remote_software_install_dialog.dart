enum RemoteSoftwareInstallerType { exe, msi, pkg, dmg }

enum RemoteSoftwareDetectionType { file, registry, process }

enum RemoteSoftwareInstallMode { silent, interactive }

class RemoteSoftwareInstallForm {
  String softwareName;
  String packageUrl;
  String sha256;
  RemoteSoftwareInstallerType installerType;
  RemoteSoftwareDetectionType detectionType;
  String detectionValue;
  String silentArgs;
  RemoteSoftwareInstallMode mode;

  RemoteSoftwareInstallForm({
    this.softwareName = '',
    this.packageUrl = '',
    this.sha256 = '',
    this.installerType = RemoteSoftwareInstallerType.exe,
    this.detectionType = RemoteSoftwareDetectionType.file,
    this.detectionValue = '',
    this.silentArgs = '',
    this.mode = RemoteSoftwareInstallMode.silent,
  });

  Map<String, String> validate() {
    final errors = <String, String>{};
    if (softwareName.trim().isEmpty) errors['softwareName'] = '软件名称不能为空';
    final uri = Uri.tryParse(packageUrl.trim());
    if (uri == null || uri.scheme != 'https' || uri.host.isEmpty) {
      errors['packageUrl'] = '安装包地址必须使用 HTTPS';
    } else if (!uri.host.contains('.') || uri.host.startsWith('.') || uri.host.endsWith('.')) {
      errors['packageUrl'] = '安装包地址域名无效';
    }
    if (!RegExp(r'^[a-fA-F0-9]{64}$').hasMatch(sha256.trim())) {
      errors['sha256'] = 'SHA-256 必须是 64 位十六进制字符串';
    }
    if (detectionValue.trim().isEmpty) errors['detectionValue'] = '检测值不能为空';
    return errors;
  }

  Map<String, dynamic> toJson() => {
        'softwareName': softwareName,
        'packageUrl': packageUrl,
        'sha256': sha256,
        'installerType': installerType.name,
        'detectionType': detectionType.name,
        'detectionValue': detectionValue,
        'silentArgs': silentArgs,
        'mode': mode.name,
      };
}

bool shouldShowRemoteSoftwareInstall(bool capability, bool permission) =>
    capability && permission;
