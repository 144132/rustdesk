enum ConnectionManagerWindowMode { hidden, minimized, visible }

bool shouldMinimizeConnectionManagerOnClose(
        Iterable<ConnectionManagerWindowMode> activeConnections) =>
    activeConnections.contains(ConnectionManagerWindowMode.minimized);

ConnectionManagerWindowMode connectionManagerWindowMode({
  required bool isWindows,
  required bool isLoggedIn,
  required bool authorized,
  required bool isRemoteDesktop,
  required bool showCmWindow,
}) {
  if (isWindows && authorized && isRemoteDesktop) {
    return isLoggedIn
        ? ConnectionManagerWindowMode.visible
        : ConnectionManagerWindowMode.minimized;
  }
  return showCmWindow
      ? ConnectionManagerWindowMode.visible
      : ConnectionManagerWindowMode.hidden;
}
