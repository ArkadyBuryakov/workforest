# Changelog

## 0.2.0

The `workforest` CLI ships with the extension: the Marketplace package for
each platform (`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`)
carries a self-contained executable, used when no `workforest` is
installed. An installed one still wins. `workforest.executable` now
defaults to empty (search) instead of `workforest`; a path set there is
still used as given.

## 0.1.0

Initial release: the Workforest sidebar — a header toolbar (create, open,
run script, checkout, delete, refresh, and more under `…`) over two
collapsible sections, Scripts (run/stop with one click) and Worktrees
(main checkout, then managed worktrees by recency; dirty markers; the
worktree this window is in) —
create/open/delete/checkout worktrees, run and stop `scripts` in the
integrated terminal, show the merged configuration, scaffold project and
`.vscode/` local configs, status bar item.
