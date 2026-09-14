"""The package scripts, through their entry points: the generator's drift check, and pack's stamping."""

from __future__ import annotations

import runpy
import shutil
from pathlib import Path

import pytest

from onemessagebus._pin import workspace_version
from scripts import generate, pack
from tests.conftest import PACKAGE, ROOT

SOURCE = PACKAGE / "src" / "onemessagebus"
VERSION = workspace_version((ROOT / "Cargo.toml").read_text(encoding="utf-8"))


def test_the_generator_is_deterministic_and_its_check_goes_red_on_a_stale_copy(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    fresh = tmp_path / "onemessagebus"
    assert generate.main(["--output", str(fresh)]) == 0
    assert generate.owned_files(fresh) == generate.owned_files(SOURCE), (
        "the committed copy is the output"
    )

    contract = fresh / "_generated" / "contract.py"
    contract.write_text(
        contract.read_text(encoding="utf-8").replace(
            "class Sent(BaseModel):", "class Sent(BaseModel):  # edited by hand", 1
        ),
        encoding="utf-8",
    )
    stray = fresh / "_generated" / "messages" / "stray.py"
    stray.write_text("STRAY = 1\n", encoding="utf-8")
    (fresh / "_generated" / "capabilities.json").unlink()
    (fresh / "models.py").write_text('"""Edited by hand."""\n', encoding="utf-8")
    capsys.readouterr()
    assert generate.main(["--check", "--output", str(fresh)]) == 1
    said = capsys.readouterr()
    assert "--- _generated/contract.py (committed)" in said.out
    assert "+++ _generated/contract.py (generated)" in said.out
    assert "-class Sent(BaseModel):  # edited by hand" in said.out
    assert "--- _generated/messages/stray.py (committed)" in said.out
    assert "+++ _generated/capabilities.json (generated)" in said.out
    assert "--- models.py (committed)" in said.out
    assert "4 generated file(s) differ from the Rust bundle; run the generator" in said.err
    assert stray.exists(), "--check writes nothing"

    assert generate.main(["--output", str(fresh)]) == 0
    assert generate.owned_files(fresh) == generate.owned_files(SOURCE)
    assert not stray.exists()


def test_pack_stamps_every_placeholder_from_the_workspace_version(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    assert pack.main(["--output", str(tmp_path)]) == 0
    destination = Path(capsys.readouterr().out.splitlines()[-1])
    assert destination == tmp_path / "onemessagebus-sdk"
    project = (destination / "pyproject.toml").read_text(encoding="utf-8")
    assert f'\nversion = "{VERSION}"\n' in project
    assert f'"onemessagebus-cli=={VERSION}"' in project
    assert "0.0.0.dev0" not in project
    assert sorted(path.name for path in destination.iterdir()) == [
        "README.md",
        "pyproject.toml",
        "src",
    ]
    constants = runpy.run_path(str(destination / "src" / "onemessagebus" / "_version.py"))
    assert (constants["__version__"], constants["CLI_VERSION"]) == (VERSION, VERSION)


def test_pack_refuses_a_placeholder_that_moved(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    source = tmp_path / "source"
    source.mkdir()
    for name in ("pyproject.toml", "README.md"):
        shutil.copy2(PACKAGE / name, source / name)
    shutil.copytree(PACKAGE / "src", source / "src", ignore=shutil.ignore_patterns("__pycache__"))
    constants = source / "src" / "onemessagebus" / "_version.py"
    constants.write_text(
        constants.read_text(encoding="utf-8").replace(
            'CLI_VERSION = "0.0.0.dev0"', 'CLI_VERSION = "0.1.0"'
        ),
        encoding="utf-8",
    )
    with pytest.raises(SystemExit) as moved:
        pack.main(["--source", str(source), "--output", str(tmp_path / "out")])
    assert moved.value.code == 1
    said = capsys.readouterr().err
    assert '_version.py holds 0 copies of `CLI_VERSION = "0.0.0.dev0"`' in said
    assert "the placeholder moved" in said

    manifest = tmp_path / "Cargo.toml"
    manifest.write_text("[workspace]\nmembers = []\n", encoding="utf-8")
    with pytest.raises(SystemExit) as unversioned:
        pack.main(["--manifest", str(manifest), "--output", str(tmp_path / "out")])
    assert unversioned.value.code == 1
    assert "declares no [workspace.package] version" in capsys.readouterr().err
