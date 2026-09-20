import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/models/server_model.dart';

Map<String, dynamic> legacyClientJson() => {
      'id': 1,
      'authorized': true,
      'is_file_transfer': false,
      'is_view_camera': false,
      'is_terminal': false,
      'port_forward': '',
      'name': 'peer',
      'avatar': '',
      'peer_id': 'peer-id',
      'keyboard': true,
      'clipboard': true,
      'audio': true,
      'file': true,
      'restart': false,
      'recording': false,
      'block_input': false,
      'privacy_mode': false,
      'disconnected': false,
      'from_switch': false,
      'in_voice_call': false,
      'incoming_voice_call': false,
    };

void main() {
  test('suppressed authorized Windows remote does not request CM window', () {
    expect(shouldShowConnectionManagerWindow(true, false, false), isFalse);
  });

  test('custom password connection keeps requesting CM window', () {
    expect(shouldShowConnectionManagerWindow(true, false, true), isTrue);
  });

  test('missing display flag keeps legacy visible behavior', () {
    final client = Client.fromJson(legacyClientJson());
    expect(client.showCmWindow, isTrue);
  });
}
