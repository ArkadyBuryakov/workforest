# Changelog

## 0.2.0

The `workforest` CLI ships with the extension: the Marketplace package for
each platform (`linux-x64`, `linux-arm64`, `darwin-x64`, `darwin-arm64`)
carries a self-contained executable, used when no `workforest` is
installed. An installed one still wins. `workforest.executable` now
defaults to empty (search) instead of `workforest`; a path set there is
still used as given.

- Scripts show a running badge: light blue in this window's worktree,
  orange (with the count) in the others.
- Checkout and Delete from the header or the Command Palette act on the
  worktree this window is in, after confirming it, instead of asking
  which; from the main checkout they still ask.

## 0.1.0

Initial release: the Workforest sidebar — a header toolbar (create, open,
run script, checkout, delete, refresh, and more under `…`) over two
collapsible sections, Scripts (run/stop with one click) and Worktrees
(main checkout, then managed worktrees by recency; dirty markers; the
worktree this window is in) —
create/open/delete/checkout worktrees, run and stop `scripts` in the
integrated terminal, show the merged configuration, scaffold project and
`.vscode/` local configs, status bar item.
