"""The fixtures' own resolution: the binary is found wherever Cargo wrote it."""

from __future__ import annotations

from pathlib import Path

import pytest

from tests.conftest import cargo_target_dir


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


def test_a_cargo_that_refuses_fails_the_run_with_what_it_said(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("ONEMESSAGEBUS_TARGET_DIR", raising=False)
    monkeypatch.setenv("CARGO_HOME", str(tmp_path))
    (tmp_path / "config.toml").write_text("[build\n", encoding="utf-8")
    with pytest.raises(pytest.fail.Exception, match="did not name Cargo's target directory"):
        cargo_target_dir()
