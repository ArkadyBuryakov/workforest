#!/bin/sh
# Build a self-contained `workforest` executable — no Python, no venv, no
# pip — so the editor plugins can ship the CLI they drive (editors/vscode,
# editors/idea). PyInstaller freezes the wheel this checkout builds, so the
# binary carries exactly the package data the wheel does (shell/,
# templates/, examples/).
#
#   packaging/binary/build.sh [OUTDIR]      # default: dist/binary
#
# The binary only runs on the OS and architecture it was built on: each of
# linux-x64, linux-arm64, darwin-x64, darwin-arm64 is built on a matching
# runner (.github/workflows/binaries.yml).
#
# Not a replacement for the packaged installs: no `wf` alias, no man pages,
# no shell-init in the user's rc. It is the plugins' fallback for people who
# have not installed workforest themselves.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/dist/binary}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# A one-line entry script instead of src/workforest/__main__.py: PyInstaller
# puts the script's own directory on sys.path, and src/workforest/ there
# would shadow the installed package's modules as top-level ones.
printf 'from workforest.cli import main\n\nraise SystemExit(main())\n' > "$work/entry.py"

# --no-project: this is the wheel's dependency set, not the dev group's.
uv run --no-project --python 3.14 --with pyinstaller --with "$root" -- \
  pyinstaller "$work/entry.py" \
    --onefile --name workforest --clean --noconfirm \
    --collect-data workforest --collect-submodules workforest \
    --exclude-module tkinter --exclude-module unittest \
    --distpath "$out" --workpath "$work/build" --specpath "$work"

"$out/workforest" --version
