# Local fork

Upstream: `herdrdev/herdr`. Fork: `jellis206/herdr`.

## Worktree placement

Without an explicit `--path`, new checkouts follow this repository's existing
Git-registered linked worktrees. Each live linked checkout votes for its parent
directory. The most common directory wins; tied directories are ranked by their
newest worktree registration. An exact timestamp tie is resolved deterministically
by directory path. Primary, bare, missing, and prunable checkouts do not vote.

Git does not record worktree creation timestamps. Registration-directory birth
time is used where available, otherwise its modification time. On filesystems
without birth times, later registration changes can affect the tie-break.

The existing branch-to-folder slug remains unchanged. An explicit `--path` wins.
If there are no eligible linked worktrees, Herdr uses its configured
`<worktrees.directory>/<repo>/<branch-slug>` default. Existing checkouts are never
moved. Listing, opening, grouping, and removal still use Git's worktree registry,
including worktrees created outside Herdr.

Implicit-path creation runs discovery first, so it can return `worktree_busy`
or `worktree_list_failed` instead of silently using the default directory.
Windows servers are untested: canonical `\\?\` paths may reach `git worktree add`.

The server performs discovery on its existing bounded background worker path.
The create dialog uses the server's inferred directory, including remote paths;
clients never inspect their own filesystem to guess a remote project's layout.

## Build and sync

Dotfiles `.config/dev_tools.json` builds this checkout into `~/.local/bin/herdr`.
Zig 0.16.0 is pinned through dotfiles' asdf toolchain configuration; `ZIG` may
explicitly override its location. Run `devtools herdr` after toolchain setup.
Do not use `herdr update` to update this fork: it installs upstream binaries.

`jellis206/fork-sync` automatically discovers non-archived forks, including this
one. It merges upstream changes daily or opens a `sync/upstream` conflict PR.
No per-repository registration is required.
