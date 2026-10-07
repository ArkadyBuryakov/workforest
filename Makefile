# Dev targets wrap cargo; a Rust toolchain (and cargo-llvm-cov, for the
# coverage gate) is the only setup.
# Machine-specific overrides (PREFIX, IDEA_JAVA_HOME, IDEA_PLUGINS) go in
# an untracked Makefile.local.
-include Makefile.local

# Where `make install` puts this checkout: bin/ and share/man/ under it.
PREFIX ?= $(HOME)/.local

# JetBrains plugin (editors/idea). Gradle runs on any JDK 17+ — an IDE's
# bundled JBR does — and fetches the JDK 21 it compiles with itself; empty
# means "whatever `java` is on the PATH". IDEA_PLUGINS is the IDE's plugins
# directory, which is what "Install Plugin from Disk" unpacks into (a
# vendor-customized IDE uses its own vendor and data-directory names).
IDEA_JAVA_HOME ?= $(JAVA_HOME)
IDEA_PLUGINS ?= $(HOME)/.local/share/JetBrains/IdeaIC2025.2

.PHONY: check test lint cov cov-html install uninstall logo binary \
	vscode vscode-build vscode-install vscode-uninstall \
	idea idea-build idea-build-full idea-install idea-uninstall plugins

# The platform directory the JetBrains plugin looks under (Bundled.kt), and
# the name CI gives the matching binary artifact.
PLATFORM := $(shell uname -s | tr '[:upper:]' '[:lower:]')-$(shell uname -m | sed -e 's/x86_64/x64/' -e 's/aarch64/arm64/')

# The one version in the repository: Cargo.toml's. The editor clients have
# none of their own: they carry a placeholder and are stamped with this at
# package time, so a client is always the release of the CLI built into it.
# Release workflows ask packaging/version the same way.
VERSION := $(shell packaging/version)
PLACEHOLDER_VERSION := 0.0.0

# Lines the test suite must reach. Forked supervisors and the terminal
# loop are counted too: the integration tests run the built binary.
COVERAGE_FLOOR := 90
MAN := packaging/pypi/data/share/man

# The changelog the clients publish is a placeholder in the repository too:
# a release is one commit to CHANGELOG.md. Stamp it in for the build and put
# the placeholder back afterwards, whether the build succeeded or not.
STAMP := packaging/changelog/generate > /dev/null
UNSTAMP := packaging/changelog/generate --placeholder > /dev/null

# Every test binary runs even when one fails: one report, not one per fix.
test:
	cargo test --locked --no-fail-fast

lint:
	cargo fmt --check
	cargo clippy --locked --all-targets -- -D warnings

# The tests, with the coverage gate.
cov:
	cargo llvm-cov --locked --fail-under-lines $(COVERAGE_FLOOR)

check: lint cov

cov-html:
	cargo llvm-cov --locked --html
	@echo "open target/llvm-cov/html/index.html"

logo:
	./assets/generate

# Install the current checkout under PREFIX (~/.local by default): the
# binary, `wf` as a link to it, and the man pages beside them, where man(1)
# finds them through $PATH.
install:
	cargo build --locked --release --bin workforest
	mkdir -p $(PREFIX)/bin $(PREFIX)/share/man/man1 $(PREFIX)/share/man/man5
	install -m755 target/release/workforest $(PREFIX)/bin/workforest
	ln -sf workforest $(PREFIX)/bin/wf
	install -m644 $(MAN)/man1/workforest.1 $(MAN)/man1/wf.1 $(PREFIX)/share/man/man1/
	install -m644 $(MAN)/man5/workforest.5 $(MAN)/man5/wf.5 $(PREFIX)/share/man/man5/
	@echo
	@echo 'workforest installed. Make sure your shell rc has:'
	@echo '  eval "$$(workforest shell-init)"'

uninstall:
	rm -f $(PREFIX)/bin/workforest $(PREFIX)/bin/wf
	rm -f $(PREFIX)/share/man/man1/workforest.1 $(PREFIX)/share/man/man1/wf.1
	rm -f $(PREFIX)/share/man/man5/workforest.5 $(PREFIX)/share/man/man5/wf.5

# --- The CLI the editor packages ship (packaging/binary) ----------------

# One release build in dist/binary/, for this machine only: CI builds all
# four platforms and packages one .vsix per platform. Both
# editor builds copy it in, so a locally installed plugin always drives the
# CLI it was built with instead of whatever is on the IDE's PATH.
binary:
	packaging/binary/build.sh

# --- VS Code extension (editors/vscode) ---------------------------------

# A fresh .vsix from this worktree, carrying the CLI built alongside it.
vscode-build:
	rm -rf editors/vscode/bin && mkdir -p editors/vscode/bin
	cp dist/binary/workforest editors/vscode/bin/workforest
	cd editors/vscode && rm -f *.vsix && npm install --no-audit --no-fund
	@$(STAMP); \
	(cd editors/vscode && npm pkg set version=$(VERSION) && npm run package); \
	status=$$?; \
	(cd editors/vscode && npm pkg set version=$(PLACEHOLDER_VERSION)); \
	$(UNSTAMP); exit $$status

vscode-install:
	@vsix=$$(ls -t editors/vscode/*.vsix 2>/dev/null | head -1); \
	[ -n "$$vsix" ] || { echo "no .vsix — run 'make vscode-build' first" >&2; exit 1; }; \
	code --install-extension "$$vsix" --force

vscode-uninstall:
	code --uninstall-extension ArkadyBuryakov.workforest-vscode

vscode: binary vscode-build vscode-install

# --- JetBrains plugin (editors/idea) ------------------------------------

# Only this machine's platform, so the zip is not the four-platform one CI
# builds; that is all a local install can run anyway.
idea-build:
	rm -rf editors/idea/bin && mkdir -p editors/idea/bin/$(PLATFORM)
	cp dist/binary/workforest editors/idea/bin/$(PLATFORM)/workforest
	@$(STAMP); \
	(cd editors/idea && JAVA_HOME="$(IDEA_JAVA_HOME)" ./gradlew --quiet -PpluginVersion=$(VERSION) buildPlugin); \
	status=$$?; $(UNSTAMP); exit $$status

# All four platforms in one zip: what CI publishes, and what the manual
# first upload to the JetBrains Marketplace needs. A local build is for the
# machine it runs on, so the other executables come from the Binaries
# workflow — pushing a branch that touches the CLI runs it, so usually the
# artifacts are already there; otherwise `gh workflow run binaries.yml
# --ref <branch>`. The newest successful run of the current branch wins,
# else the newest of any branch; BINARIES_RUN=<run id> picks one by hand.
idea-build-full:
	@command -v gh > /dev/null || { echo "the GitHub CLI (gh) fetches the other platforms' binaries" >&2; exit 1; }
	rm -rf editors/idea/bin dist/binaries
	@branch=$$(git rev-parse --abbrev-ref HEAD); \
	run=$${BINARIES_RUN:-$$(gh run list --workflow=binaries.yml --branch "$$branch" --status=success --limit=1 --json databaseId --jq '.[0].databaseId')}; \
	[ -n "$$run" ] || run=$$(gh run list --workflow=binaries.yml --status=success --limit=1 --json databaseId --jq '.[0].databaseId'); \
	[ -n "$$run" ] || { echo "no successful Binaries run — 'gh workflow run binaries.yml' first" >&2; exit 1; }; \
	sha=$$(gh run view "$$run" --json headSha --jq .headSha); \
	echo "binaries from run $$run ($$sha)"; \
	if git cat-file -e "$$sha^{commit}" 2> /dev/null; then \
		git diff --quiet "$$sha" HEAD -- src resources Cargo.toml Cargo.lock packaging/binary || \
			echo "warning: the CLI changed since that run — these executables are not this checkout's" >&2; \
	else \
		echo "warning: $$sha is not in this checkout; cannot tell whether the CLI changed since" >&2; \
	fi; \
	gh run download "$$run" --pattern 'workforest-binary-*' --dir dist/binaries
	@for target in linux-x64 linux-arm64 darwin-x64 darwin-arm64; do \
		src=dist/binaries/workforest-binary-$$target/workforest; \
		[ -f "$$src" ] || { echo "missing $$src" >&2; exit 1; }; \
		mkdir -p editors/idea/bin/$$target; \
		cp "$$src" editors/idea/bin/$$target/workforest; \
		chmod +x editors/idea/bin/$$target/workforest; \
	done
	@$(STAMP); \
	(cd editors/idea && JAVA_HOME="$(IDEA_JAVA_HOME)" ./gradlew --quiet -PpluginVersion=$(VERSION) buildPlugin); \
	status=$$?; $(UNSTAMP); exit $$status
	@ls -l editors/idea/build/distributions/workforest-idea-*.zip

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

idea: binary idea-build idea-install

plugins: vscode idea
