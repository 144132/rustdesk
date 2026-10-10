class AddressBookSortType {
  static const optionKey = 'address-book-sorting';
  static const customName = 'Custom name';
  static const remoteId = 'Remote ID';
  static const onlineStatus = 'Online status';
  static const deviceName = 'Device name';

  static const values = [customName, remoteId, onlineStatus, deviceName];
}

void sortAddressBookPeers<T>(
  List<T> peers,
  String sortBy, {
  required String Function(T) id,
  required String Function(T) alias,
  required String Function(T) hostname,
  required bool Function(T) online,
}) {
  String nameOrId(T peer, String Function(T) name) {
    final value = name(peer).trim();
    return value.isEmpty ? id(peer) : value;
  }

  peers.sort((first, second) {
    var result = 0;
    switch (sortBy) {
      case AddressBookSortType.customName:
        result = _compareNaturalText(
            nameOrId(first, alias), nameOrId(second, alias));
        break;
      case AddressBookSortType.onlineStatus:
        if (online(first) != online(second)) {
          return online(first) ? -1 : 1;
        }
        result = _compareNaturalText(
            nameOrId(first, alias), nameOrId(second, alias));
        break;
      case AddressBookSortType.deviceName:
        result = _compareNaturalText(
            nameOrId(first, hostname), nameOrId(second, hostname));
        break;
    }
    if (result != 0) return result;
    result = _compareNaturalText(id(first), id(second));
    return result != 0 ? result : id(first).compareTo(id(second));
  });
}

final _naturalTextParts = RegExp(r'[0-9]+|[^0-9]+');
final _leadingZeroes = RegExp(r'^0+');

int _compareNaturalText(String first, String second) {
  final firstParts = _naturalTextParts
      .allMatches(first.trim().toLowerCase())
      .map((match) => match[0]!)
      .toList();
  final secondParts = _naturalTextParts
      .allMatches(second.trim().toLowerCase())
      .map((match) => match[0]!)
      .toList();
  for (var i = 0; i < firstParts.length && i < secondParts.length; i++) {
    var left = firstParts[i];
    var right = secondParts[i];
    final leftIsNumber = left.codeUnitAt(0) >= 48 && left.codeUnitAt(0) <= 57;
    final rightIsNumber =
        right.codeUnitAt(0) >= 48 && right.codeUnitAt(0) <= 57;
    if (leftIsNumber && rightIsNumber) {
      left = left.replaceFirst(_leadingZeroes, '');
      right = right.replaceFirst(_leadingZeroes, '');
      final lengthOrder = left.length.compareTo(right.length);
      if (lengthOrder != 0) return lengthOrder;
    }
    final order = left.compareTo(right);
    if (order != 0) return order;
  }
  return firstParts.length.compareTo(secondParts.length);
}
