import 'package:flutter_test/flutter_test.dart';

import '../lib/desktop/cm_window_policy.dart';

void main() {
  group('Windows remote connection manager', () {
    test('signed-out accounts minimize for either password display flag', () {
      for (final showCmWindow in [false, true]) {
        expect(
          connectionManagerWindowMode(
            isWindows: true,
            isLoggedIn: false,
            authorized: true,
            isRemoteDesktop: true,
            showCmWindow: showCmWindow,
          ),
          ConnectionManagerWindowMode.minimized,
        );
      }
    });

    test('signed-in accounts show for either password display flag', () {
      for (final showCmWindow in [false, true]) {
        expect(
          connectionManagerWindowMode(
            isWindows: true,
            isLoggedIn: true,
            authorized: true,
            isRemoteDesktop: true,
            showCmWindow: showCmWindow,
          ),
          ConnectionManagerWindowMode.visible,
        );
      }
    });

    test('pending acceptance stays visible for signed-out accounts', () {
      expect(
        connectionManagerWindowMode(
          isWindows: true,
          isLoggedIn: false,
          authorized: false,
          isRemoteDesktop: true,
          showCmWindow: true,
        ),
        ConnectionManagerWindowMode.visible,
      );
    });

    test('login and logout change the presentation of an existing connection',
        () {
      for (final state in [
        (false, ConnectionManagerWindowMode.minimized),
        (true, ConnectionManagerWindowMode.visible),
        (false, ConnectionManagerWindowMode.minimized),
      ]) {
        expect(
          connectionManagerWindowMode(
            isWindows: true,
            isLoggedIn: state.$1,
            authorized: true,
            isRemoteDesktop: true,
            showCmWindow: false,
          ),
          state.$2,
        );
      }
    });
  });

  test('other connection types keep their existing display flag', () {
    for (final isLoggedIn in [false, true]) {
      expect(
        connectionManagerWindowMode(
          isWindows: true,
          isLoggedIn: isLoggedIn,
          authorized: true,
          isRemoteDesktop: false,
          showCmWindow: true,
        ),
        ConnectionManagerWindowMode.visible,
      );
      expect(
        connectionManagerWindowMode(
          isWindows: true,
          isLoggedIn: isLoggedIn,
          authorized: true,
          isRemoteDesktop: false,
          showCmWindow: false,
        ),
        ConnectionManagerWindowMode.hidden,
      );
    }
  });

  test('non-Windows connection managers keep their existing display flag', () {
    for (final isLoggedIn in [false, true]) {
      expect(
        connectionManagerWindowMode(
          isWindows: false,
          isLoggedIn: isLoggedIn,
          authorized: true,
          isRemoteDesktop: true,
          showCmWindow: false,
        ),
        ConnectionManagerWindowMode.hidden,
      );
      expect(
        connectionManagerWindowMode(
          isWindows: false,
          isLoggedIn: isLoggedIn,
          authorized: true,
          isRemoteDesktop: true,
          showCmWindow: true,
        ),
        ConnectionManagerWindowMode.visible,
      );
    }
  });
}
