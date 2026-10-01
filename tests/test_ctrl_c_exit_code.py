"""Unit tests for ``is_ctrl_c_exit`` (issue #1679)."""

from __future__ import annotations

import pytest

from tests._subprocess_helpers import is_ctrl_c_exit, normalize_exit_code


@pytest.mark.parametrize("rc", [130, 3221225786, -1073741510])
def test_ctrl_c_exit_codes_are_accepted(rc: int) -> None:
    assert is_ctrl_c_exit(rc)


@pytest.mark.parametrize("rc", [0, 1, -1, 2, 143, None])
def test_other_exit_codes_are_rejected(rc: int | None) -> None:
    assert not is_ctrl_c_exit(rc)


def test_normalize_maps_signed_status_to_unsigned() -> None:
    assert normalize_exit_code(-1073741510) == 0xC000013A
    assert normalize_exit_code(3221225786) == 0xC000013A
    assert normalize_exit_code(130) == 130
