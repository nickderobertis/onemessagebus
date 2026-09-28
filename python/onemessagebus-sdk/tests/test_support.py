"""The fixtures' own resolution: the binary is found wherever Cargo wrote it."""

from __future__ import annotations

from pathlib import Path

import pytest

from tests.conftest import HANDOFF, ROOT, cargo_target_dir


@pytest.mark.parametrize("variable", ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"])
def test_the_target_directory_is_cargos_own_wherever_it_is_configured(
    variable: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("ONEMESSAGEBUS_TARGET_DIR", raising=False)
    monkeypatch.delenv("CARGO_TARGET_DIR", raising=False)
    monkeypatch.delenv("CARGO_BUILD_TARGET_DIR", raising=False)
    monkeypatch.setenv(variable, str(tmp_path))
    assert cargo_target_dir() == tmp_path


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
    """The drift gate over the hand-off: its name is a literal in four languages."""
    justfile = (ROOT / "justfile").read_text(encoding="utf-8")
    recipe = justfile[justfile.index("\n_sdk-install-test:") :]
    recipe = recipe[: recipe.index("\n\n")]
    assert f"export {HANDOFF}\n" in recipe, "the SDK install recipe hands the directory on"
    for reader, spelling in [
        ("npm/onemessagebus-sdk/scripts/package-e2e.mjs", f"process.env.{HANDOFF}"),
        ("npm/onemessagebus-sdk/test/support.ts", f"env.{HANDOFF}"),
    ]:
        assert spelling in (ROOT / reader).read_text(encoding="utf-8"), f"{reader} reads it"


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
