import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/common/hbbs/hbbs.dart';
import 'package:flutter_hbb/models/group_access.dart';
import 'package:flutter_hbb/models/peer_model.dart';

void main() {
  group('device group editing', () {
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
  });
}
