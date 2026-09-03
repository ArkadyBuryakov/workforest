# Dev targets wrap `uv run`; `uv sync` is the only setup step.
# Machine-specific overrides (IDEA_JAVA_HOME, IDEA_PLUGINS) go in an
# untracked Makefile.local.
-include Makefile.local

# JetBrains plugin (editors/idea). Gradle runs on any JDK 17+ — an IDE's
# bundled JBR does — and fetches the JDK 21 it compiles with itself; empty
# means "whatever `java` is on the PATH". IDEA_PLUGINS is the IDE's plugins
# directory, which is what "Install Plugin from Disk" unpacks into (a
# vendor-customized IDE uses its own vendor and data-directory names).
IDEA_JAVA_HOME ?= $(JAVA_HOME)
IDEA_PLUGINS ?= $(HOME)/.local/share/JetBrains/IdeaIC2025.2

.PHONY: check test lint type cov sync install uninstall logo binary \
	vscode vscode-build vscode-install vscode-uninstall \
	idea idea-build idea-install idea-uninstall plugins

# The platform directory the JetBrains plugin looks under (Bundled.kt), and
# the name CI gives the matching binary artifact.
PLATFORM := $(shell uname -s | tr '[:upper:]' '[:lower:]')-$(shell uname -m | sed -e 's/x86_64/x64/' -e 's/aarch64/arm64/')

sync:
	uv sync

test:
	uv run pytest

lint:
	uv run ruff check .
	uv run ruff format --check .

type:
	uv run mypy

check: lint type test

cov:
	uv run pytest --cov-report=html
	@echo "open htmlcov/index.html"

logo:
	uv run ./assets/generate

# Install the current checkout as a uv tool (~/.local/bin/workforest).
# --reinstall so re-running picks up changes even without a version bump.
install:
	uv tool install --reinstall .
	@echo
	@echo 'workforest installed. Make sure your shell rc has:'
	@echo '  eval "$$(workforest shell-init)"'

uninstall:
	uv tool uninstall workforest

# --- The CLI the editor packages ship (packaging/binary) ----------------

# One self-contained executable in dist/binary/, for this machine only: CI
# builds all four platforms and packages one .vsix per platform. Both
# editor builds copy it in, so a locally installed plugin always drives the
# CLI it was built with instead of whatever is on the IDE's PATH.
binary:
	packaging/binary/build.sh

# --- VS Code extension (editors/vscode) ---------------------------------

# A fresh .vsix from this worktree, carrying the CLI built alongside it.
vscode-build: binary
	rm -rf editors/vscode/bin && mkdir -p editors/vscode/bin
	cp dist/binary/workforest editors/vscode/bin/workforest
	cd editors/vscode && rm -f *.vsix && npm install --no-audit --no-fund && npm run package

vscode-install:
	@vsix=$$(ls -t editors/vscode/*.vsix 2>/dev/null | head -1); \
	[ -n "$$vsix" ] || { echo "no .vsix — run 'make vscode-build' first" >&2; exit 1; }; \
	code --install-extension "$$vsix" --force

vscode-uninstall:
	code --uninstall-extension ArkadyBuryakov.workforest

vscode: vscode-build vscode-install

# --- JetBrains plugin (editors/idea) ------------------------------------

# Only this machine's platform, so the zip is not the four-platform one CI
# builds; that is all a local install can run anyway.
idea-build: binary
	rm -rf editors/idea/bin && mkdir -p editors/idea/bin/$(PLATFORM)
	cp dist/binary/workforest editors/idea/bin/$(PLATFORM)/workforest
	cd editors/idea && JAVA_HOME="$(IDEA_JAVA_HOME)" ./gradlew --quiet buildPlugin

# What "Install Plugin from Disk" does: unpack the zip into the plugins dir.
idea-install:
	@zip=$$(ls -t editors/idea/build/distributions/workforest-idea-*.zip 2>/dev/null | head -1); \
	[ -n "$$zip" ] || { echo "no plugin zip — run 'make idea-build' first" >&2; exit 1; }; \
	rm -rf "$(IDEA_PLUGINS)/workforest-idea"; \
	unzip -qo "$$zip" -d "$(IDEA_PLUGINS)"; \
	echo "installed $(IDEA_PLUGINS)/workforest-idea — restart the IDE to load it"

idea-uninstall:
	rm -rf "$(IDEA_PLUGINS)/workforest-idea"
	@echo "removed — restart the IDE"

idea: idea-build idea-install

plugins: vscode idea
