import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_hbb/desktop/desktop_access_policy.dart';
import 'package:window_manager/window_manager.dart';

class UninstallPage extends StatefulWidget {
  const UninstallPage({Key? key}) : super(key: key);

  @override
  State<UninstallPage> createState() => _UninstallPageState();
}

class _UninstallPageState extends State<UninstallPage> {
  final _passwordController = TextEditingController();
  String _errorText = '';
  bool _submitting = false;

  @override
  void dispose() {
    _passwordController.dispose();
    super.dispose();
  }

  Future<void> _close() async {
    await windowManager.setPreventClose(false);
    await windowManager.close();
  }

  Future<void> _submit() async {
    if (_submitting) {
      return;
    }
    if (!isValidWindowsUninstallPassword(_passwordController.text)) {
      setState(() => _errorText = '密码错误');
      return;
    }

    setState(() {
      _submitting = true;
      _errorText = '';
    });
    try {
      await Process.start(
        Platform.resolvedExecutable,
        const ['--uninstall-authorized'],
        mode: ProcessStartMode.detached,
      );
      await _close();
    } catch (e) {
      if (mounted) {
        setState(() {
          _submitting = false;
          _errorText = '启动卸载程序失败，请重试';
        });
      }
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: Card(
            margin: const EdgeInsets.all(24),
            child: Padding(
              padding: const EdgeInsets.all(24),
              child: Column(
                mainAxisSize: MainAxisSize.min,
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  const Text(
                    '卸载程序',
                    style: TextStyle(fontSize: 20),
                    textAlign: TextAlign.center,
                  ),
                  const SizedBox(height: 12),
                  const Text('请输入卸载密码后继续。'),
                  const SizedBox(height: 12),
                  TextField(
                    controller: _passwordController,
                    autofocus: true,
                    obscureText: true,
                    enabled: !_submitting,
                    onSubmitted: (_) => _submit(),
                    decoration: InputDecoration(
                      labelText: '卸载密码',
                      errorText: _errorText.isEmpty ? null : _errorText,
                    ),
                  ),
                  const SizedBox(height: 20),
                  Row(
                    mainAxisAlignment: MainAxisAlignment.end,
                    children: [
                      TextButton(
                        onPressed: _submitting ? null : _close,
                        child: const Text('取消'),
                      ),
                      const SizedBox(width: 8),
                      ElevatedButton(
                        onPressed: _submitting ? null : _submit,
                        child: const Text('确认卸载'),
                      ),
                    ],
                  ),
                ],
              ),
            ),
          ),
        ),
      ),
    );
  }
}
