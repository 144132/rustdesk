enum GroupApiResource { deviceGroups, users, deviceList }

class GroupApiRequest {
  final String path;
  final Map<String, String> queryParameters;

  const GroupApiRequest(this.path, this.queryParameters);
}

GroupApiRequest buildGroupApiRequest({
  required GroupApiResource resource,
  required bool isAdmin,
  required int current,
  required int pageSize,
}) {
  final queryParameters = <String, String>{
    'current': current.toString(),
    'pageSize': pageSize.toString(),
  };

  switch (resource) {
    case GroupApiResource.deviceGroups:
      return GroupApiRequest(
        isAdmin ? '/api/device-groups' : '/api/device-group/accessible',
        queryParameters,
      );
    case GroupApiResource.users:
      queryParameters['status'] = '1';
      if (!isAdmin) queryParameters['accessible'] = '';
      return GroupApiRequest('/api/users', queryParameters);
    case GroupApiResource.deviceList:
      queryParameters['status'] = '1';
      if (!isAdmin) queryParameters['accessible'] = '';
      return GroupApiRequest(isAdmin ? '/api/devices' : '/api/peers',
          queryParameters);
  }
}

bool isAdminFromUserInfo(Map<String, dynamic>? userInfo) {
  return userInfo?['is_admin'] == true;
}

bool isGroupCacheForRole(Map<String, dynamic> cache, bool isAdmin) {
  return cache['is_admin'] is bool && cache['is_admin'] == isAdmin;
}
