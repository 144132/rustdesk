import 'package:flutter/material.dart';

enum InstallerType { msi, exe }

enum DetectionType { msiProductCode, uninstallDisplayName, exePath }

enum InstallMode { downloadOnly, downloadAndInstall }

class RemoteSoftwareInstallForm {
  String softwareName;
  String packageUrl;
  String sha256;
  InstallerType installerType;
  DetectionType detectionType;
  String detectionValue;
  List<String> silentArgs;
  InstallMode mode;

  RemoteSoftwareInstallForm({
    this.softwareName = '',
    this.packageUrl = '',
    this.sha256 = '',
    this.installerType = InstallerType.exe,
    this.detectionType = DetectionType.exePath,
    this.detectionValue = '',
    List<String>? silentArgs,
    this.mode = InstallMode.downloadOnly,
  }) : silentArgs = List<String>.from(silentArgs ?? const <String>[]);

  /// Client-side feedback only; the FFI/Rust layer must validate again.
  Map<String, String> validate() {
    final errors = <String, String>{};

    _addControlCharacterError(errors, 'softwareName', softwareName);
    _addControlCharacterError(errors, 'packageUrl', packageUrl);
    _addControlCharacterError(errors, 'sha256', sha256);
    _addControlCharacterError(errors, 'detectionValue', detectionValue);
    for (final argument in silentArgs) {
      if (_hasControlCharacters(argument)) {
        errors['silentArgs'] = '静默参数不能包含控制字符';
        break;
      }
    }

    if (softwareName.trim().isEmpty && !errors.containsKey('softwareName')) {
      errors['softwareName'] = '软件名称不能为空';
    }

    final packageUri = Uri.tryParse(packageUrl.trim());
    if (errors.containsKey('packageUrl')) {
      // The control-character error is more specific than URL parsing errors.
    } else if (packageUri == null ||
        packageUri.scheme.toLowerCase() != 'https' ||
        !_isAllowedPackageHost(packageUri.host) ||
        packageUri.userInfo.isNotEmpty) {
      errors['packageUrl'] = '安装包地址必须是 szxinyu.com 的 HTTPS 地址';
    } else if (!_hasExpectedExtension(packageUri.path, installerType)) {
      errors['packageUrl'] = '安装包路径扩展名必须与安装器类型匹配';
    }

    if (!errors.containsKey('sha256') &&
        !RegExp(r'^[a-fA-F0-9]{64}$').hasMatch(sha256.trim())) {
      errors['sha256'] = 'SHA-256 必须是 64 位十六进制字符串';
    }

    if (installerType == InstallerType.msi &&
        silentArgs.isNotEmpty &&
        !errors.containsKey('silentArgs')) {
      errors['silentArgs'] = 'MSI 安装器不允许传入静默参数';
    }

    if (detectionValue.trim().isEmpty &&
        !errors.containsKey('detectionValue')) {
      errors['detectionValue'] = '检测值不能为空';
    } else if (!errors.containsKey('detectionValue')) {
      if (detectionType == DetectionType.msiProductCode &&
          !_isMsiProductCode(detectionValue.trim())) {
        errors['detectionValue'] = 'MSI ProductCode 格式无效';
      } else if (detectionType == DetectionType.exePath &&
          !_isWindowsAbsolutePath(detectionValue.trim())) {
        errors['detectionValue'] = 'exePath 必须是 Windows 本地盘符绝对路径';
      }
    }

    return errors;
  }

  Map<String, dynamic> toJson({String? requestId}) {
    final manifest = <String, dynamic>{
      'software_name': softwareName,
      'package_url': packageUrl,
      'sha256': sha256,
      'installer_type': _installerTypeToJson(installerType),
      'detection_type': _detectionTypeToJson(detectionType),
      'detection_value': detectionValue,
      'silent_args': List<String>.from(silentArgs),
      'mode': _installModeToJson(mode),
    };
    if (requestId != null) manifest['request_id'] = requestId;
    return manifest;
  }

  static void _addControlCharacterError(
    Map<String, String> errors,
    String field,
    String value,
  ) {
    if (_hasControlCharacters(value)) {
      errors[field] = '字段不能包含控制字符';
    }
  }

  static bool _hasControlCharacters(String value) {
    return value.runes.any(
      (rune) => rune <= 0x1f || (rune >= 0x7f && rune <= 0x9f),
    );
  }

  static bool _isAllowedPackageHost(String host) {
    final normalizedHost = host.toLowerCase();
    const rootDomain = 'szxinyu.com';
    if (normalizedHost == rootDomain) return true;
    if (!normalizedHost.endsWith('.$rootDomain')) return false;

    final subdomain = normalizedHost.substring(
      0,
      normalizedHost.length - rootDomain.length - 1,
    );
    if (subdomain.isEmpty) return false;
    return subdomain.split('.').every(
      (label) => RegExp(r'^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$').hasMatch(label),
    );
  }

  static bool _hasExpectedExtension(String path, InstallerType type) {
    final extension = type == InstallerType.msi ? '.msi' : '.exe';
    return path.toLowerCase().endsWith(extension);
  }

  static bool _isMsiProductCode(String value) {
    return RegExp(
      r'^\{[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\}$',
    ).hasMatch(value);
  }

  static bool _isWindowsAbsolutePath(String value) {
    if (value.length < 3 ||
        !_isAsciiLetter(value.codeUnitAt(0)) ||
        value[1] != ':' ||
        !_isWindowsSeparator(value[2])) {
      return false;
    }
    for (var index = 3; index < value.length; index++) {
      final character = value[index];
      if (character == '<' ||
          character == '>' ||
          character == ':' ||
          character == '"' ||
          character == '|' ||
          character == '?' ||
          character == '*') {
        return false;
      }
    }
    return true;
  }

  static bool _isAsciiLetter(int codeUnit) {
    return (codeUnit >= 0x41 && codeUnit <= 0x5a) ||
        (codeUnit >= 0x61 && codeUnit <= 0x7a);
  }

  static bool _isWindowsSeparator(String character) =>
      character == '\\' || character == '/';

  static String _installerTypeToJson(InstallerType type) {
    switch (type) {
      case InstallerType.msi:
        return 'msi';
      case InstallerType.exe:
        return 'exe';
    }
  }

  static String _detectionTypeToJson(DetectionType type) {
    switch (type) {
      case DetectionType.msiProductCode:
        return 'msi_product_code';
      case DetectionType.uninstallDisplayName:
        return 'uninstall_display_name';
      case DetectionType.exePath:
        return 'exe_path';
    }
  }

  static String _installModeToJson(InstallMode mode) {
    switch (mode) {
      case InstallMode.downloadOnly:
        return 'download_only';
      case InstallMode.downloadAndInstall:
        return 'download_and_install';
    }
  }
}

bool shouldShowRemoteSoftwareInstall(bool capability, bool permission) =>
    capability && permission;

class RemoteSoftwareInstallDialog extends StatefulWidget {
  final RemoteSoftwareInstallForm? initialForm;
  final RemoteSoftwareInstallForm? form;
  final ValueChanged<RemoteSoftwareInstallForm> onSubmit;
  final VoidCallback? onCancel;

  const RemoteSoftwareInstallDialog({
    super.key,
    required this.onSubmit,
    this.onCancel,
    this.initialForm,
    this.form,
  }) : assert(initialForm == null || form == null);

  @override
  State<RemoteSoftwareInstallDialog> createState() =>
      _RemoteSoftwareInstallDialogState();
}

class _RemoteSoftwareInstallDialogState
    extends State<RemoteSoftwareInstallDialog> {
  late final TextEditingController _softwareNameController;
  late final TextEditingController _packageUrlController;
  late final TextEditingController _sha256Controller;
  late final TextEditingController _detectionValueController;
  late final TextEditingController _silentArgsController;
  late InstallerType _installerType;
  late DetectionType _detectionType;
  late InstallMode _mode;
  Map<String, String> _errors = <String, String>{};

  @override
  void initState() {
    super.initState();
    final initial =
        widget.initialForm ?? widget.form ?? RemoteSoftwareInstallForm();
    _softwareNameController = TextEditingController(text: initial.softwareName);
    _packageUrlController = TextEditingController(text: initial.packageUrl);
    _sha256Controller = TextEditingController(text: initial.sha256);
    _detectionValueController =
        TextEditingController(text: initial.detectionValue);
    _silentArgsController =
        TextEditingController(text: initial.silentArgs.join('\n'));
    _installerType = initial.installerType;
    _detectionType = initial.detectionType;
    _mode = initial.mode;
  }

  @override
  void dispose() {
    _softwareNameController.dispose();
    _packageUrlController.dispose();
    _sha256Controller.dispose();
    _detectionValueController.dispose();
    _silentArgsController.dispose();
    super.dispose();
  }

  void _submit() {
    final form = RemoteSoftwareInstallForm(
      softwareName: _softwareNameController.text,
      packageUrl: _packageUrlController.text,
      sha256: _sha256Controller.text,
      installerType: _installerType,
      detectionType: _detectionType,
      detectionValue: _detectionValueController.text,
      silentArgs: _silentArgsController.text
          .split(RegExp(r'\r?\n'))
          .map((argument) => argument.trim())
          .where((argument) => argument.isNotEmpty)
          .toList(),
      mode: _mode,
    );
    final errors = form.validate();
    setState(() => _errors = errors);
    if (errors.isEmpty) widget.onSubmit(form);
  }

  Widget _textField({
    required String field,
    required String label,
    required TextEditingController controller,
    int maxLines = 1,
  }) {
    return Padding(
      padding: const EdgeInsets.only(bottom: 12),
      child: TextField(
        controller: controller,
        maxLines: maxLines,
        decoration: InputDecoration(
          labelText: label,
          errorText: _errors[field],
          border: const OutlineInputBorder(),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    return AlertDialog(
      title: const Text('远程软件安装'),
      content: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 520),
        child: SingleChildScrollView(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: <Widget>[
              _textField(
                field: 'softwareName',
                label: '软件名称',
                controller: _softwareNameController,
              ),
              _textField(
                field: 'packageUrl',
                label: '安装包 HTTPS 地址',
                controller: _packageUrlController,
              ),
              _textField(
                field: 'sha256',
                label: 'SHA-256',
                controller: _sha256Controller,
              ),
              DropdownButtonFormField<InstallerType>(
                value: _installerType,
                decoration: const InputDecoration(labelText: '安装器类型'),
                items: InstallerType.values
                    .map(
                      (type) => DropdownMenuItem<InstallerType>(
                        value: type,
                        child: Text(type == InstallerType.msi ? 'MSI' : 'EXE'),
                      ),
                    )
                    .toList(),
                onChanged: (value) {
                  if (value != null) setState(() => _installerType = value);
                },
              ),
              const SizedBox(height: 12),
              DropdownButtonFormField<DetectionType>(
                value: _detectionType,
                decoration: const InputDecoration(labelText: '检测类型'),
                items: DetectionType.values
                    .map(
                      (type) => DropdownMenuItem<DetectionType>(
                        value: type,
                        child: Text(_detectionTypeLabel(type)),
                      ),
                    )
                    .toList(),
                onChanged: (value) {
                  if (value != null) setState(() => _detectionType = value);
                },
              ),
              const SizedBox(height: 12),
              _textField(
                field: 'detectionValue',
                label: '检测值',
                controller: _detectionValueController,
              ),
              _textField(
                field: 'silentArgs',
                label: '静默参数（每行一个）',
                controller: _silentArgsController,
                maxLines: 3,
              ),
              DropdownButtonFormField<InstallMode>(
                value: _mode,
                decoration: const InputDecoration(labelText: '安装模式'),
                items: InstallMode.values
                    .map(
                      (mode) => DropdownMenuItem<InstallMode>(
                        value: mode,
                        child: Text(_installModeLabel(mode)),
                      ),
                    )
                    .toList(),
                onChanged: (value) {
                  if (value != null) setState(() => _mode = value);
                },
              ),
            ],
          ),
        ),
      ),
      actions: <Widget>[
        TextButton(
          onPressed: widget.onCancel ?? () => Navigator.of(context).maybePop(),
          child: const Text('取消'),
        ),
        ElevatedButton(onPressed: _submit, child: const Text('提交')),
      ],
    );
  }

  static String _detectionTypeLabel(DetectionType type) {
    switch (type) {
      case DetectionType.msiProductCode:
        return 'MSI ProductCode';
      case DetectionType.uninstallDisplayName:
        return '卸载项显示名称';
      case DetectionType.exePath:
        return 'EXE 路径';
    }
  }

  static String _installModeLabel(InstallMode mode) {
    switch (mode) {
      case InstallMode.downloadOnly:
        return '仅下载';
      case InstallMode.downloadAndInstall:
        return '下载并安装';
    }
  }
}
