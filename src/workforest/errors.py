"""Error hierarchy. cli.py maps these to messages and exit codes."""

EXIT_OK = 0
EXIT_ERROR = 1
EXIT_USAGE = 2
EXIT_CANCELLED = 3
EXIT_CONFIG = 4


class WorkforestError(Exception):
    """Operational error; the message is shown to the user."""

    exit_code = EXIT_ERROR


class UsageError(WorkforestError):
    exit_code = EXIT_USAGE


class CancelledError(WorkforestError):
    exit_code = EXIT_CANCELLED


class ConfigError(WorkforestError):
    exit_code = EXIT_CONFIG


class ScriptKilledError(WorkforestError):
    """A `wf run` command died by a signal; exits 128+N like a shell would."""

    def __init__(self, message: str, signum: int) -> None:
        super().__init__(message)
        self.exit_code = 128 + signum


class GitError(WorkforestError):
    pass


class NotARepoError(GitError):
    def __init__(self) -> None:
        super().__init__("Not inside a git repository")
