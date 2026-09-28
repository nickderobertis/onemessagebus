"""The fixtures' own resolution: the binary is found wherever Cargo wrote it."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

from tests.conftest import EXECUTABLE, HANDOFF, ROOT, built_binary, cargo_target_dir


@pytest.mark.parametrize("variable", ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"])
def test_the_target_directory_is_cargos_own_wherever_it_is_configured(
    variable: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("ONEMESSAGEBUS_TARGET_DIR", raising=False)
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    monkeypatch.delenv("CARGO_BUILD_TARGET_DIR", raising=False)
    monkeypatch.setenv(variable, str(tmp_path))
    assert cargo_target_dir() == tmp_path


def test_a_suite_run_directly_drives_the_binary_in_the_directory_cargo_names(
    binary: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (tmp_path / "debug").mkdir()
    copy = shutil.copy2(binary, tmp_path / "debug" / EXECUTABLE)
    monkeypatch.delenv(HANDOFF, raising=False)
    monkeypatch.setenv("CARGO_TARGET_DIR", str(tmp_path))
    assert built_binary() == copy
    version = subprocess.run(  # noqa: S603 - argv is the binary this test just copied and a constant flag
        [copy, "--version"], capture_output=True, text=True, check=True
    )
    assert version.stdout.startswith("onemessagebus ")


def test_a_directory_with_no_binary_in_it_fails_naming_the_build(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv(HANDOFF, raising=False)
    monkeypatch.setenv("CARGO_TARGET_DIR", str(tmp_path))
    with pytest.raises(pytest.fail.Exception) as refused:
        built_binary()
    assert str(refused.value) == (
        f"{tmp_path / 'debug' / EXECUTABLE} is not built; build it with"
        " `just nx run onemessagebus-cli:build` from the repository root"
    )


def test_a_recipes_resolution_is_taken_as_handed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("ONEMESSAGEBUS_TARGET_DIR", str(tmp_path / "handed"))
    monkeypatch.setenv("CARGO_TARGET_DIR", str(tmp_path / "ignored"))
    assert cargo_target_dir() == tmp_path / "handed"


def test_a_handed_directory_that_is_not_absolute_is_refused(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(HANDOFF, "target")
    with pytest.raises(pytest.fail.Exception, match="'target' as Cargo's target directory"):
        cargo_target_dir()


def test_the_recipe_and_every_reader_hand_the_directory_on_under_one_name() -> None:
    """The drift gate over the hand-off, whose name is a literal in three languages.

    The recipe and scripts/cargo-target-dir.mjs meet end to end in
    npm/test/cargo-target-dir.test.mjs; what no run crosses is this conftest
    reading the name the recipe exports, so that is held here.
    """
    justfile = (ROOT / "justfile").read_text(encoding="utf-8")
    recipe = justfile[justfile.index("\n_sdk-install-test:") :]
    recipe = recipe[: recipe.index("\n\n")]
    assert f"export {HANDOFF}\n" in recipe, "the SDK install recipe hands the directory on"
    assert f"env -u {HANDOFF} node scripts/cargo-target-dir.mjs" in recipe, "and resolves it fresh"
    script = (ROOT / "scripts" / "cargo-target-dir.mjs").read_text(encoding="utf-8")
    assert f'export const HANDOFF = "{HANDOFF}";' in script, "the Node resolver reads it"


def test_a_cargo_that_refuses_fails_the_run_with_what_it_said(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("ONEMESSAGEBUS_TARGET_DIR", raising=False)
    monkeypatch.setenv("CARGO_HOME", str(tmp_path))
    (tmp_path / "config.toml").write_text("[build\n", encoding="utf-8")
    with pytest.raises(pytest.fail.Exception) as refused:
        cargo_target_dir()
    assert "`cargo metadata` did not name Cargo's target directory" in str(refused.value)
    assert f"could not parse TOML configuration in `{tmp_path / 'config.toml'}`" in str(
        refused.value
    )
