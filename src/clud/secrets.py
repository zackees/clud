"""Credential storage and keyring management for clud."""

import json
from pathlib import Path
from typing import TYPE_CHECKING, Protocol

if TYPE_CHECKING:
    from cryptography.fernet import Fernet  # type: ignore[import-untyped] # pyright: ignore[reportMissingImports]


class CredentialStore(Protocol):
    """Protocol for credential storage backends."""

    def get_password(self, service: str, username: str) -> str | None:
        """Get password for service and username."""
        ...  # pragma: no cover

    def set_password(self, service: str, username: str, password: str) -> None:
        """Set password for service and username."""
        ...  # pragma: no cover


class SystemKeyring:
    """Wrapper for system keyring using the keyring library."""

    def __init__(self) -> None:
        import keyring  # type: ignore[import-untyped]

        self._keyring = keyring

    def get_password(self, service: str, username: str) -> str | None:
        """Get password from system keyring."""
        return self._keyring.get_password(service, username)

    def set_password(self, service: str, username: str, password: str) -> None:
        """Set password in system keyring."""
        self._keyring.set_password(service, username, password)


class CryptFileKeyring:
    """Wrapper for cryptfile keyring when system keyring is not available."""

    def __init__(self) -> None:
        import keyring.core  # type: ignore[import-untyped]
        from keyrings.cryptfile.cryptfile import CryptFileKeyring as CryptFileBackend  # type: ignore[import-untyped]

        self._keyring_core = keyring.core
        self._keyring_core.set_keyring(CryptFileBackend())  # type: ignore[arg-type]

    def get_password(self, service: str, username: str) -> str | None:
        """Get password from cryptfile keyring."""
        return self._keyring_core.get_password(service, username)

    def set_password(self, service: str, username: str, password: str) -> None:
        """Set password in cryptfile keyring."""
        self._keyring_core.set_password(service, username, password)


class SimpleCredentialStore:
    """Simple Fernet-based credential storage as keyring fallback."""

    def __init__(self) -> None:
        from cryptography.fernet import Fernet  # type: ignore[import-untyped] # pyright: ignore[reportMissingImports]

        self._fernet_cls = Fernet
        self.config_dir = Path.home() / ".clud"
        self.config_dir.mkdir(exist_ok=True)
        self.key_file = self.config_dir / "key.bin"
        self.creds_file = self.config_dir / "credentials.enc"
        self._ensure_key()

    def _ensure_key(self) -> None:
        """Ensure encryption key exists."""
        if not self.key_file.exists():
            key = self._fernet_cls.generate_key()
            self.key_file.write_bytes(key)
            self.key_file.chmod(0o600)

    def _get_fernet(self) -> "Fernet":  # pyright: ignore[reportUnknownParameterType]
        """Get Fernet instance with stored key."""
        key = self.key_file.read_bytes()
        return self._fernet_cls(key)

    def _load_credentials(self) -> dict[str, str]:
        """Load and decrypt credentials from file."""
        if not self.creds_file.exists():
            return {}
        try:
            fernet = self._get_fernet()
            encrypted_data = self.creds_file.read_bytes()
            decrypted_data = fernet.decrypt(encrypted_data)
            return json.loads(decrypted_data.decode())
        except Exception:
            return {}

    def _save_credentials(self, creds: dict[str, str]) -> None:
        """Encrypt and save credentials to file."""
        fernet = self._get_fernet()
        data = json.dumps(creds).encode()
        encrypted_data = fernet.encrypt(data)
        self.creds_file.write_bytes(encrypted_data)
        self.creds_file.chmod(0o600)

    def get_password(self, service: str, username: str) -> str | None:
        """Get password for service and username."""
        creds = self._load_credentials()
        return creds.get(f"{service}:{username}")

    def set_password(self, service: str, username: str, password: str) -> None:
        """Set password for service and username."""
        creds = self._load_credentials()
        creds[f"{service}:{username}"] = password
        self._save_credentials(creds)


def get_credential_store() -> CredentialStore | None:
    """Get the best available credential store, trying in order of preference."""
    # Try system keyring first
    try:
        return SystemKeyring()
    except ImportError:
        pass

    # Try cryptfile keyring
    try:
        return CryptFileKeyring()
    except ImportError:
        pass

    # Fall back to simple credential store
    try:
        return SimpleCredentialStore()
    except ImportError:
        pass

    # No credential storage available
    return None
