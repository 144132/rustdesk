#!/usr/bin/env python3
# -*- coding: utf-8 -*-

import importlib.util
from pathlib import Path


MSI_DIR = Path(__file__).resolve().parent
WORKFLOW = MSI_DIR.parent.parent / ".github" / "workflows" / "flutter-build.yml"


def load_preprocess():
    spec = importlib.util.spec_from_file_location("msi_preprocess", MSI_DIR / "preprocess.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_display_name_placeholder_escapes_xml_attribute_text():
    preprocess = load_preprocess()

    result = preprocess.replace_display_name_placeholder(
        'Value="__MSI_DISPLAY_NAME__"', 'A&B <client> "quoted"'
    )

    assert result == 'Value="A&amp;B &lt;client&gt; &quot;quoted&quot;"'


def test_display_name_defaults_to_app_name():
    preprocess = load_preprocess()

    args = preprocess.make_parser().parse_args(["--app-name", "RDAPPNAM"])

    assert preprocess.resolve_display_name(args) == "RDAPPNAM"


def test_msi_templates_use_display_name_only_for_visible_shortcuts():
    wxl = (MSI_DIR / "Package" / "Language" / "Package.en-us.wxl").read_text(encoding="utf-8")
    folders = (MSI_DIR / "Package" / "Components" / "Folders.wxs").read_text(encoding="utf-8")
    rustdesk = (MSI_DIR / "Package" / "Components" / "RustDesk.wxs").read_text(encoding="utf-8")

    assert wxl.count("__MSI_DISPLAY_NAME__") == 6
    assert 'Directory Id="App.StartMenu" Name="__MSI_DISPLAY_NAME__"' in folders
    assert 'Value="$(var.Product)"' not in folders
    assert 'Value="__MSI_DISPLAY_NAME__ Tray"' in rustdesk
    assert 'Value="$(var.Product).exe"' in rustdesk


def test_workflow_passes_separate_display_names_for_package_and_template():
    workflow = WORKFLOW.read_text(encoding="utf-8")

    assert 'python preprocess.py --arp -d ../../rustdesk --display-name "新育智慧校园远程协助"' in workflow
    assert "python preprocess.py --arp --template --revision-version 0 -d ../../rustdesk-msi-template --app-name RDAPPNAM --display-name RDAPPNAM" in workflow

