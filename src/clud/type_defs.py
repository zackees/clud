"""Type definitions for clud."""

from enum import Enum


class BuildMode(Enum):
    """Build mode for compilation."""

    DEBUG = "debug"
    RELEASE = "release"
    QUICK = "quick"


class Platform(Enum):
    """Target platform for compilation."""

    WASM = "wasm"
    NATIVE = "native"


class CompileResult:
    """Result of a compilation operation."""

    def __init__(self, success: bool, message: str = "", output: str = "", error: str = "") -> None:
        self.success = success
        self.message = message
        self.output = output
        self.error = error


class CompileServerError(Exception):
    """Exception raised by compile server operations."""

    pass
