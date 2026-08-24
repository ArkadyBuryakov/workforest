"""output: prompt semantics (Ctrl-C aborts, Ctrl-D declines)."""

from collections.abc import Callable

import pytest

from workforest import output
from workforest.errors import CancelledError

Tty = Callable[[list[str]], None]


def _raise(exc: type[BaseException]) -> Callable[[], str]:
    def _inner() -> str:
        raise exc

    return _inner


class TestConfirm:
    def test_yes(self, tty: Tty) -> None:
        tty(["y"])
        assert output.confirm("Delete?") is True

    def test_empty_answer_declines(self, tty: Tty) -> None:
        tty([""])
        assert output.confirm("Delete?") is False

    def test_ctrl_c_aborts_the_command_not_just_the_question(
        self, tty: Tty, monkeypatch: pytest.MonkeyPatch
    ) -> None:
        tty([])
        monkeypatch.setattr("builtins.input", _raise(KeyboardInterrupt))
        with pytest.raises(CancelledError):
            output.confirm("Delete?")

    def test_ctrl_d_declines(self, tty: Tty, monkeypatch: pytest.MonkeyPatch) -> None:
        tty([])
        monkeypatch.setattr("builtins.input", _raise(EOFError))
        assert output.confirm("Delete?") is False

    def test_no_terminal_raises_with_force_hint(self) -> None:
        with pytest.raises(CancelledError, match="--force"):
            output.confirm("Delete?")


class TestAsk:
    def test_returns_stripped_line(self, tty: Tty) -> None:
        tty(["  answer  "])
        assert output.ask("Name?") == "answer"

    def test_ctrl_c_cancels(self, tty: Tty, monkeypatch: pytest.MonkeyPatch) -> None:
        tty([])
        monkeypatch.setattr("builtins.input", _raise(KeyboardInterrupt))
        with pytest.raises(CancelledError):
            output.ask("Name?")
