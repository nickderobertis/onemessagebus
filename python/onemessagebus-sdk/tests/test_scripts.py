"""The package scripts: the generator's drift check, and the version stamping of a packed copy."""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

from onemessagebus._pin import workspace_version
from tests.conftest import PACKAGE, ROOT

GENERATED = PACKAGE / "src" / "onemessagebus" / "_generated"
VERSION = workspace_version((ROOT / "Cargo.toml").read_text(encoding="utf-8"))


def script(name: str, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(PACKAGE / "scripts" / name), *args],
        capture_output=True,
        text=True,
        check=False,
        cwd=PACKAGE,
    )


def files(directory: Path) -> dict[str, bytes]:
    return {
        path.relative_to(directory).as_posix(): path.read_bytes()
        for path in sorted(directory.rglob("*"))
        if path.is_file() and "__pycache__" not in path.parts
    }


def test_the_generator_is_deterministic_and_its_check_goes_red_on_a_stale_copy(
    tmp_path: Path,
) -> None:
    fresh = tmp_path / "_generated"
    written = script("generate.py", "--output", str(fresh))
    assert written.returncode == 0, written.stderr
    assert files(fresh) == files(GENERATED), "the committed copy is the generator's output"

    contract = fresh / "contract.py"
    contract.write_text(
        contract.read_text(encoding="utf-8").replace(
            "class Sent(BaseModel):", "class Sent(BaseModel):  # edited by hand", 1
        ),
        encoding="utf-8",
    )
    (fresh / "messages" / "stray.py").write_text("STRAY = 1\n", encoding="utf-8")
    (fresh / "capabilities.json").unlink()
    checked = script("generate.py", "--check", "--output", str(fresh))
    assert checked.returncode == 1
    assert "--- _generated/contract.py (committed)" in checked.stdout
    assert "+++ _generated/contract.py (generated)" in checked.stdout
    assert "-class Sent(BaseModel):  # edited by hand" in checked.stdout
    assert "--- _generated/messages/stray.py (committed)" in checked.stdout
    assert "+++ _generated/capabilities.json (generated)" in checked.stdout
    assert "3 generated file(s) differ from the Rust bundle; run the generator" in checked.stderr
    assert (fresh / "messages" / "stray.py").exists(), "--check writes nothing"

    repaired = script("generate.py", "--output", str(fresh))
    assert repaired.returncode == 0, repaired.stderr
    assert files(fresh) == files(GENERATED)


def test_pack_stamps_every_placeholder_from_the_workspace_version(tmp_path: Path) -> None:
    packed = script("pack.py", "--output", str(tmp_path))
    assert packed.returncode == 0, packed.stderr
    destination = Path(packed.stdout.strip())
    assert destination == tmp_path / "onemessagebus-sdk"
    project = (destination / "pyproject.toml").read_text(encoding="utf-8")
    assert f'\nversion = "{VERSION}"\n' in project
    assert f'"onemessagebus-cli=={VERSION}"' in project
    assert "0.0.0.dev0" not in project
    constants = (destination / "src" / "onemessagebus" / "_version.py").read_text(encoding="utf-8")
    assert f'__version__ = "{VERSION}"' in constants
    assert f'CLI_VERSION = "{VERSION}"' in constants
    assert sorted(path.name for path in destination.iterdir()) == [
        "README.md",
        "pyproject.toml",
        "src",
    ]
    stamped = subprocess.run(
        [sys.executable, "-c", "import onemessagebus as m; print(m.__version__, m.CLI_VERSION)"],
        capture_output=True,
        text=True,
        check=True,
        env={"PYTHONPATH": str(destination / "src")},
    )
    assert stamped.stdout.split() == [VERSION, VERSION]


def test_pack_refuses_a_placeholder_that_moved(tmp_path: Path) -> None:
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
    moved = script("pack.py", "--source", str(source), "--output", str(tmp_path / "out"))
    assert moved.returncode == 1
    assert '_version.py holds 0 copies of `CLI_VERSION = "0.0.0.dev0"`' in moved.stderr
    assert "the placeholder moved" in moved.stderr

    manifest = tmp_path / "Cargo.toml"
    manifest.write_text("[workspace]\nmembers = []\n", encoding="utf-8")
    unversioned = script("pack.py", "--manifest", str(manifest), "--output", str(tmp_path / "out"))
    assert unversioned.returncode == 1
    assert "declares no [workspace.package] version" in unversioned.stderr
