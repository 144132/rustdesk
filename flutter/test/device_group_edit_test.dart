import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/common/hbbs/hbbs.dart';
import 'package:flutter_hbb/common/widgets/device_group_picker.dart';
import 'package:flutter_hbb/models/group_access.dart';
import 'package:flutter_hbb/models/peer_model.dart';

void main() {
  group('device group editing', () {
    testWidgets(
        'device group picker keeps its choices clickable above an overlay dialog',
        (tester) async {
      var selected = 'none';
      final overlayKey = GlobalKey<OverlayState>();

      await tester.pumpWidget(
        MaterialApp(
          home: Overlay(
            key: overlayKey,
            initialEntries: [
              OverlayEntry(
                builder: (_) => const ModalBarrier(
                  dismissible: false,
                  color: Colors.black45,
                ),
              ),
              OverlayEntry(
                builder: (_) => Center(
                  child: DeviceGroupPicker(
                    label: 'Select device group',
                    value: 'none',
                    items: const [
                      DeviceGroupPickerItem(value: 'none', label: 'Unassign'),
                      DeviceGroupPickerItem(value: 'school', label: 'School'),
                    ],
                    onChanged: (value) => selected = value,
                  ),
                ),
              ),
            ],
          ),
        ),
      );

      await tester.tap(find.text('Select device group'));
      await tester.pumpAndSettle();
      expect(find.text('School'), findsOneWidget);

      await tester.tap(find.text('School'));
      await tester.pumpAndSettle();
      expect(selected, 'school');
    });

    test('uses the admin peer update endpoint', () {
      final request = buildGroupApiMutationRequest(
        mutation: GroupApiMutation.updatePeerGroup,
      );

      expect(request.path, '/api/admin/peer/update');
      expect(request.usesAdminToken, isTrue);
    });

    test('builds a peer update payload with row and group ids', () {
      expect(
        buildAdminPeerGroupUpdatePayload(rowId: 17, groupId: 4),
        {'row_id': 17, 'group_id': 4},
      );
    });

    test('keeps the server row and group ids when mapping admin peers', () {
      final normalized = normalizeAdminPeerPayload({
        'id': 'peer-1',
        'row_id': 17,
        'group_id': 4,
        'hostname': 'PC-01',
      });
      final peer = PeerPayload.fromJson(normalized);

      final mapped = PeerPayload.toPeer(peer);

      expect(mapped.serverRowId, 17);
      expect(mapped.deviceGroupId, 4);
    });

    test('maps string server ids from an admin response', () {
      final peer = PeerPayload.fromJson({
        'id': 'peer-1',
        'row_id': '17',
        'group_id': '4',
        'info': {
          'device_name': 'PC-01',
          'os': 'Windows',
          'username': 'eric',
        },
      });

      final mapped = PeerPayload.toPeer(peer);

      expect(mapped.serverRowId, 17);
      expect(mapped.deviceGroupId, 4);
    });

    test('falls back to a standard device-group guid when no numeric id exists', () {
      final group = DeviceGroupPayload.fromJson({
        'guid': 'group-guid',
        'name': '办公室',
      });

      expect(group.id, 'group-guid');
    });

    test('keeps the admin numeric id when a guid is also present', () {
      final group = DeviceGroupPayload.fromJson({
        'id': 4,
        'guid': 'group-guid',
        'name': '办公室',
      });

      expect(group.id, '4');
    });

    test('keeps server identifiers in the group cache', () {
      final peer = Peer.fromJson({
        'id': 'peer-1',
        'server_row_id': 17,
        'device_group_id': 4,
      });

      expect(peer.toGroupCacheJson(), {
        'id': 'peer-1',
        'username': '',
        'hostname': '',
        'platform': '',
        'login_name': '',
        'device_group_name': '',
        'server_row_id': 17,
        'device_group_id': 4,
      });
    });
  });
}
