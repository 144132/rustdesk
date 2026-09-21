import 'package:flutter/material.dart';

class DeviceGroupPickerItem {
  final String value;
  final String label;

  const DeviceGroupPickerItem({required this.value, required this.label});
}

/// A dropdown that inserts its menu into the app Overlay.
///
/// The Windows desktop dialogs in this app are themselves OverlayEntries. The
/// route used by Flutter's DropdownButton can therefore be painted below the
/// dialog. Keeping the menu in the same Overlay guarantees that it is painted
/// after the dialog and remains clickable.
class DeviceGroupPicker extends StatefulWidget {
  final String label;
  final String value;
  final List<DeviceGroupPickerItem> items;
  final ValueChanged<String>? onChanged;
  final bool enabled;

  const DeviceGroupPicker({
    Key? key,
    required this.label,
    required this.value,
    required this.items,
    required this.onChanged,
    this.enabled = true,
  }) : super(key: key);

  @override
  State<DeviceGroupPicker> createState() => _DeviceGroupPickerState();
}

class _DeviceGroupPickerState extends State<DeviceGroupPicker> {
  final _targetKey = GlobalKey();
  final _layerLink = LayerLink();
  OverlayEntry? _menuEntry;
  bool _menuOpen = false;

  DeviceGroupPickerItem? get _selectedItem {
    for (final item in widget.items) {
      if (item.value == widget.value) return item;
    }
    return null;
  }

  void _toggleMenu() {
    if (!widget.enabled || widget.items.isEmpty) return;
    if (_menuEntry == null) {
      _showMenu();
    } else {
      _hideMenu();
    }
  }

  void _showMenu() {
    final overlay = Overlay.maybeOf(context, rootOverlay: true);
    if (overlay == null) return;

    final renderObject = _targetKey.currentContext?.findRenderObject();
    final double targetWidth =
        renderObject is RenderBox ? renderObject.size.width : 0.0;
    final double width = targetWidth > 0 ? targetWidth : 240.0;

    _menuEntry = OverlayEntry(
      builder: (context) => Stack(
        children: [
          Positioned.fill(
            child: GestureDetector(
              behavior: HitTestBehavior.translucent,
              onTap: _hideMenu,
            ),
          ),
          CompositedTransformFollower(
            link: _layerLink,
            showWhenUnlinked: false,
            targetAnchor: Alignment.bottomLeft,
            followerAnchor: Alignment.topLeft,
            offset: const Offset(0, 4),
            child: Material(
              elevation: 8,
              color: Theme.of(context).cardColor,
              borderRadius: BorderRadius.circular(4),
              clipBehavior: Clip.antiAlias,
              child: ConstrainedBox(
                constraints: const BoxConstraints(maxHeight: 280),
                child: SizedBox(
                  width: width,
                  child: ListView.builder(
                    padding: EdgeInsets.zero,
                    shrinkWrap: true,
                    itemCount: widget.items.length,
                    itemBuilder: (context, index) {
                      final item = widget.items[index];
                      final isSelected = item.value == widget.value;
                      return InkWell(
                        onTap: () {
                          widget.onChanged?.call(item.value);
                          _hideMenu();
                        },
                        child: Container(
                          constraints: const BoxConstraints(minHeight: 44),
                          alignment: Alignment.centerLeft,
                          padding: const EdgeInsets.symmetric(horizontal: 16),
                          color: isSelected
                              ? Theme.of(context)
                                  .colorScheme
                                  .primary
                                  .withOpacity(0.12)
                              : null,
                          child: Text(
                            item.label,
                            overflow: TextOverflow.ellipsis,
                          ),
                        ),
                      );
                    },
                  ),
                ),
              ),
            ),
          ),
        ],
      ),
    );
    overlay.insert(_menuEntry!);
    if (mounted) setState(() => _menuOpen = true);
  }

  void _hideMenu() {
    _menuEntry?.remove();
    _menuEntry = null;
    if (mounted && _menuOpen) setState(() => _menuOpen = false);
  }

  @override
  void didUpdateWidget(covariant DeviceGroupPicker oldWidget) {
    super.didUpdateWidget(oldWidget);
    if ((!widget.enabled || widget.items.isEmpty) && _menuEntry != null) {
      _hideMenu();
    }
  }

  @override
  void dispose() {
    _menuEntry?.remove();
    _menuEntry = null;
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final selectedItem = _selectedItem;
    final enabled = widget.enabled && widget.items.isNotEmpty;

    return CompositedTransformTarget(
      link: _layerLink,
      child: Semantics(
        button: true,
        enabled: enabled,
        expanded: _menuOpen,
        child: GestureDetector(
          key: _targetKey,
          onTap: enabled ? _toggleMenu : null,
          child: InputDecorator(
            isFocused: _menuOpen,
            isEmpty: selectedItem == null,
            decoration: InputDecoration(
              labelText: widget.label,
              enabled: enabled,
              border: const OutlineInputBorder(),
              focusedBorder: OutlineInputBorder(
                borderSide: BorderSide(
                  color: Theme.of(context).colorScheme.primary,
                  width: 2,
                ),
              ),
            ),
            child: Row(
              children: [
                Expanded(
                  child: Text(
                    selectedItem?.label ?? '',
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
                const Icon(Icons.arrow_drop_down),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
