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

      expect(request.path, '/api/device-groups');
      expect(request.queryParameters, {
        'current': '3',
        'pageSize': '100',
      });
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

      expect(adminRequest.path, '/api/devices');
      expect(adminRequest.queryParameters, {
        'current': '4',
        'pageSize': '100',
      });
      expect(ordinaryRequest.queryParameters, {
        'current': '4',
        'pageSize': '100',
        'accessible': '',
        'status': '1',
      });
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

      expect(adminDeviceUri.path, '/proxy/api/devices');
      expect(adminDeviceUri.queryParameters, {
        'current': '1',
        'pageSize': '100',
      });
      expect(adminGroupUri.path, '/proxy/api/device-groups');
      expect(ordinaryUserUri.path, '/proxy/api/users');
      expect(ordinaryUserUri.queryParameters['accessible'], '');
      expect(ordinaryUserUri.queryParameters['status'], '1');
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
