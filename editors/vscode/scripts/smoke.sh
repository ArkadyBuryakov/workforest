#!/bin/sh
# Run the extension inside a real VS Code against a scratch forest.
# Needs `code` on PATH and a display; uses this checkout's workforest via
# its .venv (`uv sync` first). Everything lives under $SMOKE_DIR (default: a
# temp dir); the in-host test appends its log to $SMOKE_DIR/smoke.log.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
root=$(cd "$here/../.." && pwd)
dir=${SMOKE_DIR:-$(mktemp -d)}
export SMOKE_DIR="$dir"
rm -rf "$dir/smoke" "$dir/worktrees" "$dir/user-data" "$dir/extensions"
mkdir -p "$dir/smoke" "$dir/user-data/User" "$dir/extensions"

git -c init.defaultBranch=main init -q "$dir/smoke"
cd "$dir/smoke"
git -c user.name=smoke -c user.email=smoke@example.invalid commit -q --allow-empty -m init
printf 'scripts:\n  hello: echo hello\n' > .workforest.yaml
git add .workforest.yaml
git -c user.name=smoke -c user.email=smoke@example.invalid commit -q -m config
"$root/.venv/bin/workforest" create feat --no-open --no-hooks
touch "$dir/worktrees/smoke/feat/dirty.txt"

printf '{"workforest.executable": "%s"}\n' "$root/.venv/bin/workforest" > "$dir/user-data/User/settings.json"

cd "$here"
: > "$dir/smoke.log"
code --new-window --wait \
  --user-data-dir="$dir/user-data" --extensions-dir="$dir/extensions" \
  --disable-extensions --disable-gpu --disable-workspace-trust \
  --extensionDevelopmentPath="$here" --extensionTestsPath="$here/out/smoke" \
  "$dir/smoke"
status=$?
cat "$dir/smoke.log"
exit $status
