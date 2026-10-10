import '../../lib/models/address_book_sort.dart';

typedef _Peer = ({String id, String alias, String hostname, bool online});

_Peer _peer(String id,
        {String alias = '', String hostname = '', bool online = false}) =>
    (id: id, alias: alias, hostname: hostname, online: online);

void _sort(List<_Peer> peers, String sortBy) {
  sortAddressBookPeers<_Peer>(
    peers,
    sortBy,
    id: (peer) => peer.id,
    alias: (peer) => peer.alias,
    hostname: (peer) => peer.hostname,
    online: (peer) => peer.online,
  );
}

void _expectIds(List<_Peer> peers, List<String> expected) {
  final actual = peers.map((peer) => peer.id).toList();
  if (actual.length != expected.length ||
      Iterable.generate(actual.length).any((i) => actual[i] != expected[i])) {
    throw StateError('Expected IDs $expected, got $actual');
  }
}

void runAddressBookSortChecks(
    void Function(String, void Function()) registerTest) {
  registerTest('custom names use natural order for Chinese numbered names', () {
    final peers = [
      _peer('3', alias: '圆10'),
      _peer('2', alias: '圆2'),
      _peer('1', alias: '圆1'),
    ];
    _sort(peers, AddressBookSortType.customName);
    _expectIds(peers, ['1', '2', '3']);
  });

  registerTest('custom names ignore case and surrounding whitespace', () {
    final peers = [
      _peer('30', alias: ' PC10 '),
      _peer('20', alias: 'pc2'),
      _peer('10', alias: 'PC1'),
    ];
    _sort(peers, AddressBookSortType.customName);
    _expectIds(peers, ['10', '20', '30']);
  });

  registerTest('custom names fall back to ID when blank', () {
    final peers = [
      _peer('10'),
      _peer('3', alias: '1'),
      _peer('2', alias: '  '),
    ];
    _sort(peers, AddressBookSortType.customName);
    _expectIds(peers, ['3', '2', '10']);
  });

  registerTest('ID order ignores conflicting custom names', () {
    final peers = [
      _peer('10', alias: 'A'),
      _peer('2', alias: 'Z'),
      _peer('1', alias: 'M'),
    ];
    _sort(peers, AddressBookSortType.remoteId);
    _expectIds(peers, ['1', '2', '10']);
  });

  registerTest('ID order supports numbers longer than machine integers', () {
    final peers = [
      _peer('100000000000000000000000000000000000000'),
      _peer('90000000000000000000000000000000000000'),
      _peer('2'),
    ];
    _sort(peers, AddressBookSortType.remoteId);
    _expectIds(peers, [
      '2',
      '90000000000000000000000000000000000000',
      '100000000000000000000000000000000000000',
    ]);
  });

  registerTest('online peers precede offline peers with name order within each',
      () {
    final peers = [
      _peer('1', alias: '圆1'),
      _peer('4', alias: '圆10', online: true),
      _peer('2', alias: '圆2'),
      _peer('3', alias: '圆2', online: true),
    ];
    _sort(peers, AddressBookSortType.onlineStatus);
    _expectIds(peers, ['3', '4', '1', '2']);
  });

  registerTest('online order is refreshed after an online state change', () {
    final peers = [
      _peer('1', alias: 'A', online: true),
      _peer('2', alias: 'B'),
    ];
    _sort(peers, AddressBookSortType.onlineStatus);
    _expectIds(peers, ['1', '2']);
    peers[0] = _peer('1', alias: 'A');
    peers[1] = _peer('2', alias: 'B', online: true);
    _sort(peers, AddressBookSortType.onlineStatus);
    _expectIds(peers, ['2', '1']);
  });

  registerTest('device names use hostname rather than alias or ID', () {
    final peers = [
      _peer('1', alias: 'A', hostname: 'PC10'),
      _peer('3', alias: 'Z', hostname: 'pc1'),
      _peer('2', alias: 'B', hostname: ' PC2 '),
    ];
    _sort(peers, AddressBookSortType.deviceName);
    _expectIds(peers, ['3', '2', '1']);
  });

  registerTest('device names fall back to ID when blank', () {
    final peers = [
      _peer('10', alias: 'A'),
      _peer('3', hostname: '1'),
      _peer('2', alias: 'Z', hostname: '  '),
    ];
    _sort(peers, AddressBookSortType.deviceName);
    _expectIds(peers, ['3', '2', '10']);
  });

  registerTest('equal names have deterministic ID order across refreshes', () {
    for (final sortBy in AddressBookSortType.values) {
      final peers = [
        _peer('10', alias: 'same', hostname: 'same', online: true),
        _peer('2', alias: 'SAME', hostname: 'SAME', online: true),
        _peer('1', alias: 'same', hostname: 'same', online: true),
      ];
      _sort(peers, sortBy);
      _expectIds(peers, ['1', '2', '10']);
      final reversed = peers.reversed.toList();
      _sort(reversed, sortBy);
      _expectIds(reversed, ['1', '2', '10']);
    }
  });

  registerTest('multiple numeric segments and leading zeroes sort naturally',
      () {
    final peers = [
      _peer('4', alias: 'PC10-1'),
      _peer('3', alias: 'PC2-10'),
      _peer('2', alias: 'PC02-2'),
      _peer('1', alias: 'PC2-1'),
    ];
    _sort(peers, AddressBookSortType.customName);
    _expectIds(peers, ['1', '2', '3', '4']);
  });

  registerTest('unknown saved sort options fall back to ID order', () {
    final peers = [_peer('10', alias: 'A'), _peer('2', alias: 'Z')];
    _sort(peers, 'unsupported');
    _expectIds(peers, ['2', '10']);
  });

  registerTest('empty and single-peer address books can use every sort option',
      () {
    for (final sortBy in AddressBookSortType.values) {
      final empty = <_Peer>[];
      _sort(empty, sortBy);
      _expectIds(empty, []);
      final single = [_peer('1')];
      _sort(single, sortBy);
      _expectIds(single, ['1']);
    }
  });
}
