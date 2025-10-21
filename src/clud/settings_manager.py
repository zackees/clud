"""Settings management for clud - handles persistent user preferences."""

import json
from pathlib import Path
from typing import Any


def get_settings_file() -> Path:
    """Get the path to the settings file."""
    config_dir = Path.home() / ".clud"
    config_dir.mkdir(exist_ok=True)
    return config_dir / "settings.json"


def load_settings() -> dict[str, Any]:
    """Load settings from settings.json.

    Returns:
        Dictionary of settings, empty dict if file doesn't exist
    """
    settings_file = get_settings_file()
    if not settings_file.exists():
        return {}

    try:
        with open(settings_file, encoding="utf-8") as f:
            data: Any = json.load(f)
            if isinstance(data, dict):
                return data  # type: ignore
            return {}
    except (OSError, json.JSONDecodeError) as e:
        print(f"Warning: Could not load settings file: {e}")
        return {}


def save_settings(settings: dict[str, Any]) -> None:
    """Save settings to settings.json.

    Args:
        settings: Dictionary of settings to save
    """
    settings_file = get_settings_file()
    try:
        with open(settings_file, "w", encoding="utf-8") as f:
            json.dump(settings, f, indent=2)
    except OSError as e:
        print(f"Warning: Could not save settings file: {e}")


def get_setting(key: str, default: str | None = None) -> str | None:
    """Get a specific setting.

    Args:
        key: The setting key
        default: Default value if key doesn't exist

    Returns:
        The setting value or default
    """
    settings = load_settings()
    value = settings.get(key, default)
    return value if isinstance(value, str) else default


def set_setting(key: str, value: str) -> None:
    """Set a specific setting.

    Args:
        key: The setting key
        value: The value to set
    """
    settings = load_settings()
    settings[key] = value
    save_settings(settings)


def get_model_preference() -> str | None:
    """Get the saved model preference.

    Returns:
        The model flag (e.g., '--haiku', '--sonnet') or None if not set
    """
    return get_setting("model", None)


def set_model_preference(model: str) -> None:
    """Save the model preference.

    Args:
        model: The model flag to save (e.g., '--haiku', '--sonnet')
    """
    set_setting("model", model)
