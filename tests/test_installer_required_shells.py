"""#1467: mandatory installer lanes cannot silently skip supported shells."""

from pathlib import Path

import pytest

from tests import test_installer_native_entry as native

SHELL_CASES = [
    native.test_bash_fresh_login_and_interactive_lookup,
    native.test_existing_user_bin_and_first_bash_login_file,
    native.test_fish_custom_xdg_uses_nonuniversal_snippet,
    native.test_zsh_custom_zdotdir_activation,
]


@pytest.mark.parametrize("scenario", SHELL_CASES, ids=lambda case: case.__name__)
def test_required_acceptance_fails_when_a_supported_shell_is_missing(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, scenario
) -> None:
    monkeypatch.setenv("CLUD_REQUIRE_INSTALLER_SHELLS", "1")
    monkeypatch.setattr(native.shutil, "which", lambda _name: None)
    with pytest.raises(pytest.fail.Exception, match="required installer acceptance shell"):
        try:
            scenario(tmp_path)
        except pytest.skip.Exception as error:
            raise AssertionError("mandatory acceptance skipped its supported shell") from error


@pytest.mark.parametrize("scenario", SHELL_CASES, ids=lambda case: case.__name__)
def test_optional_developer_acceptance_can_skip_an_unavailable_shell(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, scenario
) -> None:
    monkeypatch.delenv("CLUD_REQUIRE_INSTALLER_SHELLS", raising=False)
    monkeypatch.setattr(native.shutil, "which", lambda _name: None)
    with pytest.raises(pytest.skip.Exception, match="unavailable"):
        scenario(tmp_path)
