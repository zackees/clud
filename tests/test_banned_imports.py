"""Tests for the narrow running-process command-builder exception."""

from pathlib import Path

import pytest

from ci.banned_imports import COMMAND_BUILDER_MARKER, is_allowed, scan_file


def test_marked_command_builder_is_allowed() -> None:
    marker = f"// {COMMAND_BUILDER_MARKER}"
    assert is_allowed("use std::process::Command;", marker)
    assert is_allowed("let mut command = Command::new(program);", marker)


def test_command_builder_exception_does_not_allow_raw_spawn() -> None:
    marker = f"// {COMMAND_BUILDER_MARKER}"
    assert not is_allowed(
        "let child = Command::new(program).spawn().expect(\"spawn\");",
        marker,
    )


def test_unmarked_std_command_remains_banned() -> None:
    assert not is_allowed("use std::process::Command;")
    assert not is_allowed("let mut command = Command::new(program);")


@pytest.mark.parametrize("method", ["spawn", "status", "output"])
def test_scan_rejects_multiline_raw_execution(tmp_path: Path, method: str) -> None:
    source = tmp_path / "raw.rs"
    source.write_text(
        f"""// {COMMAND_BUILDER_MARKER}
let mut command = Command::new(program);
command
    .{method}()
    .expect("raw execution");
""",
        encoding="utf-8",
    )
    violations = scan_file(source)
    assert any("hand std::process::Command" in reason for _, _, reason in violations)


def test_marker_does_not_hide_another_banned_construct(tmp_path: Path) -> None:
    source = tmp_path / "mixed.rs"
    source.write_text(
        (
            "let mut command = Command::new(program); "
            f"let _ = std::process::Stdio::null(); // {COMMAND_BUILDER_MARKER}\n"
        ),
        encoding="utf-8",
    )
    violations = scan_file(source)
    assert violations


@pytest.mark.parametrize(
    "mention",
    [
        "// Never call command.spawn() directly",
        'let warning = "command.status() is forbidden";',
        'let warning = r#"command.output() is forbidden"#;',
        "/* Never call command.spawn() directly */",
    ],
)
def test_execution_mentions_in_comments_and_strings_are_ignored(
    tmp_path: Path, mention: str
) -> None:
    source = tmp_path / "mention.rs"
    source.write_text(
        f"""// {COMMAND_BUILDER_MARKER}
let mut command = Command::new(program);
{mention}
running_process::spawn(&mut command, stdio)?;
""",
        encoding="utf-8",
    )
    assert scan_file(source) == []


def test_char_literal_cannot_hide_later_raw_execution(tmp_path: Path) -> None:
    source = tmp_path / "char_then_spawn.rs"
    source.write_text(
        f"""// {COMMAND_BUILDER_MARKER}
let mut command = Command::new(program);
let quote = '\"';
command.spawn()?;
""",
        encoding="utf-8",
    )
    violations = scan_file(source)
    assert any("hand std::process::Command" in reason for _, _, reason in violations)
