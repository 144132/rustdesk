import 'package:flutter_test/flutter_test.dart';
import 'package:flutter_hbb/common/hbbs/hbbs.dart';
import 'package:flutter_hbb/models/group_access.dart';

void main() {
  group('group API access selection', () {
    test('admin requests all device groups with pagination', () {
      final request = buildGroupApiRequest(
        resource: GroupApiResource.deviceGroups,
        isAdmin: true,
        current: 3,
        pageSize: 100,
      );

      expect(request.path, '/api/admin/device_group/list');
      expect(request.queryParameters, {
        'page': '3',
        'page_size': '100',
      });
      expect(request.usesAdminToken, isTrue);
    });

    test('ordinary users request accessible device groups with pagination', () {
      final request = buildGroupApiRequest(
        resource: GroupApiResource.deviceGroups,
        isAdmin: false,
        current: 2,
        pageSize: 50,
      );

      expect(request.path, '/api/device-group/accessible');
      expect(request.queryParameters, {
        'current': '2',
        'pageSize': '50',
      });
      expect(request.usesAdminToken, isFalse);
    });

    test('admin user requests all users without access or status filters', () {
      final request = buildGroupApiRequest(
        resource: GroupApiResource.users,
        isAdmin: true,
        current: 2,
        pageSize: 100,
      );

      expect(request.path, '/api/users');
      expect(request.queryParameters, {
        'current': '2',
        'pageSize': '100',
      });
      expect(request.usesAdminToken, isFalse);
    });

    test('ordinary user request keeps accessible, status, and pagination', () {
      final request = buildGroupApiRequest(
        resource: GroupApiResource.users,
        isAdmin: false,
        current: 1,
        pageSize: 25,
      );

      expect(request.path, '/api/users');
      expect(request.queryParameters, {
        'current': '1',
        'pageSize': '25',
        'accessible': '',
        'status': '1',
      });
      expect(request.usesAdminToken, isFalse);
    });

    test('admin device request is full while ordinary peer request keeps accessible', () {
      final adminRequest = buildGroupApiRequest(
        resource: GroupApiResource.deviceList,
        isAdmin: true,
        current: 4,
        pageSize: 100,
      );
      final ordinaryRequest = buildGroupApiRequest(
        resource: GroupApiResource.deviceList,
        isAdmin: false,
        current: 4,
        pageSize: 100,
      );

      expect(adminRequest.path, '/api/admin/peer/list');
      expect(adminRequest.queryParameters, {
        'page': '4',
        'page_size': '100',
        'id': '',
        'hostname': '',
        'username': '',
        'ip': '',
      });
      expect(adminRequest.usesAdminToken, isTrue);
      expect(ordinaryRequest.queryParameters, {
        'current': '4',
        'pageSize': '100',
        'accessible': '',
        'status': '1',
      });
      expect(ordinaryRequest.usesAdminToken, isFalse);
    });

    test('preserves API server base path for admin and ordinary endpoints', () {
      final baseUri = Uri.parse('https://host/proxy');
      final adminDeviceUri = buildGroupApiUri(
        baseUri,
        buildGroupApiRequest(
          resource: GroupApiResource.deviceList,
          isAdmin: true,
          current: 1,
          pageSize: 100,
        ),
      );
      final adminGroupUri = buildGroupApiUri(
        baseUri,
        buildGroupApiRequest(
          resource: GroupApiResource.deviceGroups,
          isAdmin: true,
          current: 2,
          pageSize: 100,
        ),
      );
      final ordinaryUserUri = buildGroupApiUri(
        baseUri,
        buildGroupApiRequest(
          resource: GroupApiResource.users,
          isAdmin: false,
          current: 3,
          pageSize: 25,
        ),
      );

      expect(adminDeviceUri.path, '/proxy/api/admin/peer/list');
      expect(adminDeviceUri.queryParameters, {
        'page': '1',
        'page_size': '100',
        'id': '',
        'hostname': '',
        'username': '',
        'ip': '',
      });
      expect(adminGroupUri.path, '/proxy/api/admin/device_group/list');
      expect(ordinaryUserUri.path, '/proxy/api/users');
      expect(ordinaryUserUri.queryParameters['accessible'], '');
      expect(ordinaryUserUri.queryParameters['status'], '1');
    });

    test('uses api-token for admin endpoints and bearer token for client endpoints', () {
      final adminRequest = buildGroupApiRequest(
        resource: GroupApiResource.deviceList,
        isAdmin: true,
        current: 1,
        pageSize: 100,
      );
      final ordinaryRequest = buildGroupApiRequest(
        resource: GroupApiResource.deviceList,
        isAdmin: false,
        current: 1,
        pageSize: 100,
      );

      expect(buildGroupApiHeaders('token', adminRequest), {'api-token': 'token'});
      expect(buildGroupApiHeaders('token', ordinaryRequest), {
        'Authorization': 'Bearer token',
      });
    });

    test('unwraps the admin response envelope', () {
      final response = normalizeGroupApiResponse({
        'code': 0,
        'message': 'success',
        'data': {
          'page': 1,
          'page_size': 100,
          'total': 1,
          'list': [
            {'name': '办公室'},
          ],
        },
      }, adminEndpoint: true);

      expect(response['total'], 1);
      expect(response['data'], [
        {'name': '办公室'},
      ]);
    });

    test('maps an admin peer record to the client peer payload', () {
      final payload = normalizeAdminPeerPayload({
        'id': 'peer-1',
        'hostname': 'PC-01',
        'os': 'Windows 11',
        'username': 'eric',
        'user': {'name': '管理员'},
        'alias': '办公室电脑',
      });

      expect(payload['id'], 'peer-1');
      expect(payload['info'], {
        'device_name': 'PC-01',
        'os': 'Windows',
        'username': 'eric',
      });
      expect(payload['user_name'], '管理员');
      expect(payload['note'], '办公室电脑');
    });
  });

  group('admin role persistence and cache isolation', () {
    test('persists is_admin in user info', () {
      final user = UserPayload.fromJson({
        'name': 'admin',
        'is_admin': true,
      });

      expect(user.toJson()['is_admin'], isTrue);
      expect(isAdminFromUserInfo(user.toJson()), isTrue);
    });

    test('does not reuse a cache created for another role', () {
      expect(isGroupCacheForRole({'is_admin': true}, true), isTrue);
      expect(isGroupCacheForRole({'is_admin': true}, false), isFalse);
      expect(isGroupCacheForRole({'is_admin': false}, true), isFalse);
      expect(isGroupCacheForRole({'is_admin': false}, false), isTrue);
      expect(isGroupCacheForRole({}, false), isFalse);
    });
  });
}
