"""Tests for skill auto-installer module."""

import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import MagicMock, patch

from clud.skill_installer import (
    CURRENT_SKILLS_VERSION,
    MANAGED_HEADER,
    install_skills,
    is_clud_managed,
    needs_install,
    uninstall_skills,
)


class TestNeedsInstall(unittest.TestCase):
    """Test first-run detection via needs_install()."""

    def setUp(self) -> None:
        """Create a temporary directory for settings."""
        self.tmp_dir = tempfile.mkdtemp()
        self.settings_file = Path(self.tmp_dir) / "settings.json"

    def tearDown(self) -> None:
        """Clean up temporary directory."""
        shutil.rmtree(self.tmp_dir, ignore_errors=True)

    @patch("clud.skill_installer.get_settings_file")
    def test_needs_install_no_settings_file(self, mock_settings_file: MagicMock) -> None:
        """Returns True when settings file doesn't exist (first run)."""
        mock_settings_file.return_value = self.settings_file
        self.assertTrue(needs_install())

    @patch("clud.skill_installer.get_settings_file")
    def test_needs_install_no_version_key(self, mock_settings_file: MagicMock) -> None:
        """Returns True when settings exist but no skills_version key."""
        self.settings_file.write_text('{"model": "--sonnet"}', encoding="utf-8")
        mock_settings_file.return_value = self.settings_file
        self.assertTrue(needs_install())

    @patch("clud.skill_installer.get_settings_file")
    def test_needs_install_outdated_version(self, mock_settings_file: MagicMock) -> None:
        """Returns True when installed version is older than current."""
        self.settings_file.write_text('{"skills_version": "0.0.1"}', encoding="utf-8")
        mock_settings_file.return_value = self.settings_file
        self.assertTrue(needs_install())

    @patch("clud.skill_installer.get_settings_file")
    def test_needs_install_current_version(self, mock_settings_file: MagicMock) -> None:
        """Returns False when installed version matches current."""
        self.settings_file.write_text(
            f'{{"skills_version": "{CURRENT_SKILLS_VERSION}"}}',
            encoding="utf-8",
        )
        mock_settings_file.return_value = self.settings_file
        self.assertTrue(not needs_install())


class TestIsCludManaged(unittest.TestCase):
    """Test detection of clud-managed files via header comment."""

    def setUp(self) -> None:
        self.tmp_dir = tempfile.mkdtemp()

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp_dir, ignore_errors=True)

    def test_managed_file_detected(self) -> None:
        """File with managed header is detected as clud-managed."""
        f = Path(self.tmp_dir) / "test.md"
        f.write_text(f"{MANAGED_HEADER}\n# My Agent\nDoes stuff.", encoding="utf-8")
        self.assertTrue(is_clud_managed(f))

    def test_user_file_not_detected(self) -> None:
        """File without managed header is NOT detected as clud-managed."""
        f = Path(self.tmp_dir) / "test.md"
        f.write_text("# My Custom Agent\nI wrote this myself.", encoding="utf-8")
        self.assertFalse(is_clud_managed(f))

    def test_nonexistent_file(self) -> None:
        """Non-existent file returns False."""
        f = Path(self.tmp_dir) / "nope.md"
        self.assertFalse(is_clud_managed(f))


class TestInstallSkills(unittest.TestCase):
    """Test that install_skills copies assets to ~/.claude/."""

    def setUp(self) -> None:
        """Create temp dirs for both ~/.clud (settings) and ~/.claude (target)."""
        self.tmp_clud = tempfile.mkdtemp()  # ~/.clud equivalent
        self.tmp_claude = tempfile.mkdtemp()  # ~/.claude equivalent
        self.settings_file = Path(self.tmp_clud) / "settings.json"

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp_clud, ignore_errors=True)
        shutil.rmtree(self.tmp_claude, ignore_errors=True)

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_installs_agent_files(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """Agent markdown files are copied to ~/.claude/agents/."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file
        install_skills(quiet=True)

        agents_dir = Path(self.tmp_claude) / "agents"
        self.assertTrue(agents_dir.exists())
        agent_files = list(agents_dir.glob("*.md"))
        self.assertGreater(len(agent_files), 0, "Should install at least one agent")

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_installs_skill_files(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """Skill SKILL.md files are copied to ~/.claude/skills/<name>/."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file
        install_skills(quiet=True)

        skills_dir = Path(self.tmp_claude) / "skills"
        self.assertTrue(skills_dir.exists())
        skill_files = list(skills_dir.rglob("SKILL.md"))
        self.assertGreater(len(skill_files), 0, "Should install at least one skill")

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_installs_rule_files(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """Rule markdown files are copied to ~/.claude/rules/."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file
        install_skills(quiet=True)

        rules_dir = Path(self.tmp_claude) / "rules"
        self.assertTrue(rules_dir.exists())
        rule_files = list(rules_dir.glob("*.md"))
        self.assertGreater(len(rule_files), 0, "Should install at least one rule")

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_sets_version_after_install(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """skills_version is written to settings.json after install."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file
        install_skills(quiet=True)

        import json

        data = json.loads(self.settings_file.read_text(encoding="utf-8"))
        self.assertEqual(data["skills_version"], CURRENT_SKILLS_VERSION)

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_installed_files_have_managed_header(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """All installed files contain the managed-by-clud header."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file
        install_skills(quiet=True)

        for md_file in Path(self.tmp_claude).rglob("*.md"):
            content = md_file.read_text(encoding="utf-8")
            self.assertTrue(
                content.startswith(MANAGED_HEADER),
                f"{md_file.name} missing managed header",
            )

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_does_not_clobber_user_files(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """Files edited by the user (no managed header) are not overwritten."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file

        # First install
        install_skills(quiet=True)

        # Find an installed agent and simulate user editing it
        agents_dir = Path(self.tmp_claude) / "agents"
        agent_files = list(agents_dir.glob("*.md"))
        self.assertGreater(len(agent_files), 0)

        user_content = "# My Custom Version\nI edited this myself."
        agent_files[0].write_text(user_content, encoding="utf-8")

        # Second install (upgrade scenario)
        install_skills(quiet=True)

        # User's file should be untouched
        self.assertEqual(agent_files[0].read_text(encoding="utf-8"), user_content)

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_updates_managed_files(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """Files still marked as managed ARE updated on re-install."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file

        # First install
        install_skills(quiet=True)

        # Find an installed agent and verify it has managed header
        agents_dir = Path(self.tmp_claude) / "agents"
        agent_files = list(agents_dir.glob("*.md"))
        self.assertGreater(len(agent_files), 0)
        original_content = agent_files[0].read_text(encoding="utf-8")
        self.assertTrue(original_content.startswith(MANAGED_HEADER))

        # Corrupt the file but keep the managed header
        corrupted = MANAGED_HEADER + "\nCORRUPTED"
        agent_files[0].write_text(corrupted, encoding="utf-8")

        # Re-install should overwrite
        install_skills(quiet=True)
        restored = agent_files[0].read_text(encoding="utf-8")
        self.assertEqual(restored, original_content)


class TestUninstallSkills(unittest.TestCase):
    """Test that uninstall_skills removes managed files and clears version."""

    def setUp(self) -> None:
        self.tmp_clud = tempfile.mkdtemp()
        self.tmp_claude = tempfile.mkdtemp()
        self.settings_file = Path(self.tmp_clud) / "settings.json"

    def tearDown(self) -> None:
        shutil.rmtree(self.tmp_clud, ignore_errors=True)
        shutil.rmtree(self.tmp_claude, ignore_errors=True)

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_removes_managed_files(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """Uninstall removes only clud-managed files."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file

        # Install first
        install_skills(quiet=True)

        # Add a user file that should NOT be removed
        user_file = Path(self.tmp_claude) / "agents" / "my-custom-agent.md"
        user_file.write_text("# My agent\nCustom.", encoding="utf-8")

        # Uninstall
        uninstall_skills(quiet=True)

        # User file should survive
        self.assertTrue(user_file.exists())

        # Managed files should be gone
        managed_files = [f for f in Path(self.tmp_claude).rglob("*.md") if is_clud_managed(f)]
        self.assertEqual(len(managed_files), 0, "All managed files should be removed")

    @patch("clud.skill_installer.get_settings_file")
    @patch("clud.skill_installer._get_claude_dir")
    def test_clears_version_in_settings(self, mock_claude_dir: MagicMock, mock_settings_file: MagicMock) -> None:
        """Uninstall removes skills_version from settings."""
        mock_claude_dir.return_value = Path(self.tmp_claude)
        mock_settings_file.return_value = self.settings_file

        install_skills(quiet=True)
        uninstall_skills(quiet=True)

        import json

        data = json.loads(self.settings_file.read_text(encoding="utf-8"))
        self.assertNotIn("skills_version", data)


if __name__ == "__main__":
    unittest.main()
