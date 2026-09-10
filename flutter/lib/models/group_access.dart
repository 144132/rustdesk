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
      if (!isAdmin) {
        queryParameters['accessible'] = '';
        queryParameters['status'] = '1';
      }
      return GroupApiRequest('/api/users', queryParameters);
    case GroupApiResource.deviceList:
      if (isAdmin) {
        return GroupApiRequest('/api/devices', queryParameters);
      }
      queryParameters['accessible'] = '';
      queryParameters['status'] = '1';
      return GroupApiRequest('/api/peers', queryParameters);
  }
}

Uri buildGroupApiUri(Uri baseUri, GroupApiRequest request) {
  var basePath = baseUri.path;
  if (basePath == '/') basePath = '';
  if (basePath.endsWith('/')) {
    basePath = basePath.substring(0, basePath.length - 1);
  }
  return baseUri.replace(
    path: '$basePath${request.path}',
    queryParameters: request.queryParameters,
  );
}

bool isAdminFromUserInfo(Map<String, dynamic>? userInfo) {
  return userInfo?['is_admin'] == true;
}

bool isGroupCacheForRole(Map<String, dynamic> cache, bool isAdmin) {
  return cache['is_admin'] is bool && cache['is_admin'] == isAdmin;
}
