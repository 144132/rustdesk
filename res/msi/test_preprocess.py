#!/usr/bin/env python3
# -*- coding: utf-8 -*-

import importlib.util
import re
import sys
from pathlib import Path


MSI_DIR = Path(__file__).resolve().parent
WORKFLOW = MSI_DIR.parent.parent / ".github" / "workflows" / "flutter-build.yml"
CARGO_TOML = MSI_DIR.parent.parent / "Cargo.toml"
CARGO_LOCK = MSI_DIR.parent.parent / "Cargo.lock"


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


def test_app_name_replacement_remains_for_non_display_wxl_text(tmp_path, monkeypatch):
    preprocess = load_preprocess()
    msi_dir = tmp_path / "msi"
    language_dir = msi_dir / "Package" / "Language"
    language_dir.mkdir(parents=True)
    language_file = language_dir / "Package.en-us.wxl"
    language_file.write_text(
        '<String Id="F_App" Value="RustDesk" />\n'
        '<String Id="SC_Client" Value="__MSI_DISPLAY_NAME__" />\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(sys, "argv", [str(msi_dir / "preprocess.py")])

    preprocess.replace_app_name_in_langs("AcmeDesk")

    result = language_file.read_text(encoding="utf-8")
    assert 'Value="AcmeDesk"' in result
    assert "__MSI_DISPLAY_NAME__" in result


def test_display_name_defaults_to_app_name():
    preprocess = load_preprocess()

    args = preprocess.make_parser().parse_args(["--app-name", "RDAPPNAM"])

    assert preprocess.resolve_display_name(args) == "RDAPPNAM"


def test_msi_templates_use_display_name_only_for_visible_shortcuts():
    wxl = (MSI_DIR / "Package" / "Language" / "Package.en-us.wxl").read_text(encoding="utf-8")
    package = (MSI_DIR / "Package" / "Package.wxs").read_text(encoding="utf-8")
    folders = (MSI_DIR / "Package" / "Components" / "Folders.wxs").read_text(encoding="utf-8")
    rustdesk = (MSI_DIR / "Package" / "Components" / "RustDesk.wxs").read_text(encoding="utf-8")

    assert wxl.count("__MSI_DISPLAY_NAME__") == 6
    assert 'WixLocalization Culture="en-us" Codepage="936"' in wxl
    assert 'String Id="SummaryCodepage" Value="936"' in wxl
    assert 'Scope="perMachine" Codepage="936"' in package
    assert 'Directory Id="App.StartMenu" Name="__MSI_DISPLAY_NAME__"' in folders
    assert 'Value="$(var.Product)"' not in folders
    assert 'Value="__MSI_DISPLAY_NAME__ Tray"' in rustdesk
    assert 'Value="$(var.Product).exe"' in rustdesk


def test_workflow_passes_separate_display_names_for_package_and_template():
    workflow = WORKFLOW.read_text(encoding="utf-8")

    assert 'python preprocess.py --arp -d ../../rustdesk --display-name "新育智慧校园远程协助"' in workflow
    assert "python preprocess.py --arp --template --revision-version 0 -d ../../rustdesk-msi-template --app-name RDAPPNAM --display-name RDAPPNAM" in workflow


def test_release_version_is_cargo_compatible_and_consistent_across_build_inputs():
    cargo = CARGO_TOML.read_text(encoding="utf-8")
    lock = CARGO_LOCK.read_text(encoding="utf-8")
    workflow = WORKFLOW.read_text(encoding="utf-8")

    cargo_version = re.search(r'^version\s*=\s*"([^"]+)"', cargo, re.MULTILINE).group(1)
    lock_version = re.search(
        r'name = "rustdesk"\s+version = "([^"]+)"', lock, re.MULTILINE
    ).group(1)
    workflow_version = re.search(r'^  VERSION:\s*"([^"]+)"', workflow, re.MULTILINE).group(1)

    assert re.fullmatch(r"\d+\.\d+\.\d+-\d+", cargo_version)
    assert lock_version == cargo_version
    assert workflow_version == cargo_version


def test_android_release_builds_use_a_new_version_name_and_version_code():
    workflow = WORKFLOW.read_text(encoding="utf-8")

    assert 'ANDROID_BUILD_NUMBER: "20260912"' in workflow
    assert workflow.count('--build-name "${{ env.VERSION }}"') == 5
    assert workflow.count('--build-number "${{ env.ANDROID_BUILD_NUMBER }}"') == 5
