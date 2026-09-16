enum GroupApiResource { deviceGroups, users, deviceList }

class GroupApiRequest {
  final String path;
  final Map<String, String> queryParameters;
  final bool usesAdminToken;

  const GroupApiRequest(
    this.path,
    this.queryParameters, {
    this.usesAdminToken = false,
  });
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
      if (isAdmin) {
        return GroupApiRequest(
          '/api/admin/device_group/list',
          {
            'page': current.toString(),
            'page_size': pageSize.toString(),
          },
          usesAdminToken: true,
        );
      }
      return GroupApiRequest('/api/device-group/accessible', queryParameters);
    case GroupApiResource.users:
      if (!isAdmin) {
        queryParameters['accessible'] = '';
        queryParameters['status'] = '1';
      }
      return GroupApiRequest('/api/users', queryParameters);
    case GroupApiResource.deviceList:
      if (isAdmin) {
        return GroupApiRequest(
          '/api/admin/peer/list',
          {
            'page': current.toString(),
            'page_size': pageSize.toString(),
            'id': '',
            'hostname': '',
            'username': '',
            'ip': '',
          },
          usesAdminToken: true,
        );
      }
      queryParameters['accessible'] = '';
      queryParameters['status'] = '1';
      return GroupApiRequest('/api/peers', queryParameters);
  }
}

Map<String, String> buildGroupApiHeaders(
    String accessToken, GroupApiRequest request) {
  if (request.usesAdminToken) {
    return {'api-token': accessToken};
  }
  return {'Authorization': 'Bearer $accessToken'};
}

String _adminPeerGroupName(
  Map<String, dynamic> peer,
  Map<String, String> deviceGroupNamesById,
) {
  final explicitGroupName =
      (peer['device_group_name'] ?? peer['group_name'] ?? '').toString();
  if (explicitGroupName.trim().isNotEmpty) {
    return explicitGroupName;
  }
  final groupId = peer['group_id']?.toString() ?? '';
  return deviceGroupNamesById[groupId] ?? '';
}

bool _adminPeerHasGroup(
  Map<String, dynamic> peer,
  Map<String, String> deviceGroupNamesById,
) {
  final rawGroupId = peer['group_id'];
  final groupId = rawGroupId is num
      ? rawGroupId.toInt()
      : int.tryParse(rawGroupId?.toString() ?? '');
  return (groupId != null && groupId > 0) ||
      _adminPeerGroupName(peer, deviceGroupNamesById).trim().isNotEmpty;
}

int _adminPeerRecordOrder(Map<String, dynamic> peer) {
  final rawRowId = peer['row_id'];
  final rowId = rawRowId is num
      ? rawRowId.toInt()
      : int.tryParse(rawRowId?.toString() ?? '');
  if (rowId != null) return rowId;

  final rawLastOnline = peer['last_online_time'];
  return rawLastOnline is num
      ? rawLastOnline.toInt()
      : int.tryParse(rawLastOnline?.toString() ?? '') ?? 0;
}

bool _shouldPreferAdminPeer(
  Map<String, dynamic> candidate,
  Map<String, dynamic> current, {
  required Map<String, String> deviceGroupNamesById,
}) {
  final candidateHasGroup =
      _adminPeerHasGroup(candidate, deviceGroupNamesById);
  final currentHasGroup = _adminPeerHasGroup(current, deviceGroupNamesById);
  if (candidateHasGroup != currentHasGroup) {
    return candidateHasGroup;
  }
  return _adminPeerRecordOrder(candidate) >=
      _adminPeerRecordOrder(current);
}

List<Map<String, dynamic>> mergeAdminPeerRecordsById(
  Iterable<Map<String, dynamic>> records, {
  Map<String, String> deviceGroupNamesById = const <String, String>{},
}) {
  // The admin endpoint returns peer records, while the client identifies a
  // device by its RustDesk ID. Keep one canonical record per device ID;
  // assigned-group rows win over ungrouped historical rows.
  final peersById = <String, Map<String, dynamic>>{};
  for (final record in records) {
    final id = (record['id'] ?? '').toString().trim();
    if (id.isEmpty) continue;
    final current = peersById[id];
    if (current == null ||
        _shouldPreferAdminPeer(
          record,
          current,
          deviceGroupNamesById: deviceGroupNamesById,
        )) {
      peersById[id] = record;
    }
  }
  return peersById.values.toList();
}

Map<String, dynamic> normalizeGroupApiResponse(
    Map<String, dynamic> json, {
    required bool adminEndpoint,
  }) {
  if (!adminEndpoint) {
    return json;
  }

  final payload = json['data'];
  if (payload is! Map) {
    return json;
  }

  return <String, dynamic>{
    ...json,
    'total': payload['total'] ?? 0,
    'data': payload['list'] is List ? payload['list'] : <dynamic>[],
  };
}

String _normalizeAdminPeerOs(dynamic value) {
  final os = value?.toString() ?? '';
  final lowerOs = os.toLowerCase();
  if (lowerOs.contains('windows')) {
    return 'Windows';
  }
  if (lowerOs.contains('mac')) {
    return 'macOS';
  }
  if (lowerOs.contains('android')) {
    return 'Android';
  }
  if (lowerOs.contains('linux')) {
    return 'Linux';
  }
  return os;
}

Map<String, dynamic> normalizeAdminPeerPayload(
  Map<String, dynamic> peer, {
  Map<String, String> deviceGroupNamesById = const <String, String>{},
}) {
  var userName = '';
  final user = peer['user'];
  if (user is Map) {
    userName = (user['name'] ?? user['username'] ?? '').toString();
  }
  if (userName.isEmpty) {
    userName = (peer['user_name'] ?? '').toString();
  }
  final deviceGroupName =
      _adminPeerGroupName(peer, deviceGroupNamesById);

  return <String, dynamic>{
    'id': peer['id'] ?? '',
    'info': <String, dynamic>{
      'device_name': peer['hostname'] ?? '',
      'os': _normalizeAdminPeerOs(peer['os']),
      'username': peer['username'] ?? '',
    },
    'user_name': userName,
    'device_group_name': deviceGroupName,
    'note': peer['alias'] ?? '',
  };
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
