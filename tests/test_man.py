"""Man pages: hand-written roff under man/, kept honest against cli.py and
shipped through the wheel's share/man data."""

import argparse
import shutil
import subprocess
import tomllib
from pathlib import Path

import pytest

from workforest import cli

ROOT = Path(__file__).parent.parent
MAN_DIR = ROOT / "man"
PAGES = sorted(MAN_DIR.iterdir())


def roff(text: str) -> str:
    """Escape a literal the way the pages spell it (hyphens as `\\-`)."""
    return text.replace("-", "\\-")


class TestSyncWithCli:
    """The page documents what the parser accepts — no more, no less."""

    page = (MAN_DIR / "workforest.1").read_text()

    def subparsers(self) -> dict[str, argparse.ArgumentParser]:
        parser = cli.build_parser()
        action = next(a for a in parser._actions if isinstance(a, argparse._SubParsersAction))
        return dict(action.choices)

    def test_every_subcommand_has_a_section(self) -> None:
        for name in cli.SUBCOMMAND_HELP:
            assert f"\n.SS {name}" in self.page, f"no .SS section for {name!r}"

    def test_every_option_is_documented(self) -> None:
        for name, sub in self.subparsers().items():
            for action in sub._actions:
                for flag in action.option_strings:
                    if flag in ("-h", "--help"):
                        continue
                    assert roff(flag) in self.page, f"{name} {flag} missing from workforest.1"

    def test_no_phantom_options(self) -> None:
        """Every `--long` option the page mentions exists on some subparser."""
        known = {
            flag
            for sub in self.subparsers().values()
            for action in sub._actions
            for flag in action.option_strings
        } | {"--version", "--help"}
        # `.B \-\-name` / `\-\-name` inside running text
        documented = set()
        for token in self.page.replace("\\-", "-").split():
            token = token.strip("[](),.;'\"|")
            if token.startswith("--") and token[2:].replace("-", "").isalpha():
                documented.add(token)
        assert documented <= known, documented - known

    def test_exit_codes_match_errors_module(self) -> None:
        from workforest import errors

        section = self.page.split(".SH EXIT STATUS")[1].split(".SH")[0]
        for code in (
            errors.EXIT_OK,
            errors.EXIT_ERROR,
            errors.EXIT_USAGE,
            errors.EXIT_CANCELLED,
            errors.EXIT_CONFIG,
        ):
            assert f"\n.B {code}\n" in section


class TestPackaging:
    @pytest.mark.parametrize("section", ["1", "5"])
    def test_wf_is_a_link_to_workforest(self, section: str) -> None:
        link = (MAN_DIR / f"wf.{section}").read_text()
        assert link == f".so man{section}/workforest.{section}\n"

    def test_every_page_ships_in_the_wheel(self) -> None:
        pyproject = tomllib.loads((ROOT / "pyproject.toml").read_text())
        shared = pyproject["tool"]["hatch"]["build"]["targets"]["wheel"]["shared-data"]
        for page in PAGES:
            section = page.suffix[1:]
            assert shared[f"man/{page.name}"] == f"share/man/man{section}/{page.name}"
        assert len(shared) == len(PAGES)


@pytest.mark.skipif(shutil.which("groff") is None, reason="groff not installed")
@pytest.mark.parametrize("page", [p for p in PAGES if p.stem != "wf"], ids=lambda p: p.name)
def test_roff_is_clean(page: Path) -> None:
    """groff with every warning enabled must stay silent: the pages are read
    by mandoc on macOS too, which is stricter than groff's defaults."""
    result = subprocess.run(
        ["groff", "-man", "-Tutf8", "-ww", "-z", str(page)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 0
    assert result.stderr == ""
