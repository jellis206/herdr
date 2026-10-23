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
It uses the asdf default Zig; `ZIG` may explicitly override its location. Run
`devtools herdr` after toolchain setup. Do not use `herdr update` to update this
fork: it installs upstream binaries.

## Zig 0.17 and libghostty-vt

This fork builds with Zig 0.17. `vendor/libghostty-vt` is re-vendored from
Ghostty `main` (see `vendor/libghostty-vt.vendor.json`), which requires the
same Zig minor version; upstream Herdr still pins Zig 0.16 and an older Ghostty.
Herdr's local patches were rebased onto the new base and renumber their C enum
values past upstream's (see `vendor/libghostty-vt.patches.md`). CI workflows and
`AGENTS.md` still say 0.16 to keep upstream merges small.

To refresh: re-run `scripts/vendor_libghostty_vt.py --source-repo <ghostty>`,
reapply the patches, run `scripts/generate_libghostty_bindings.sh` (bindgen
0.72.1), then `just test`. An upstream Herdr re-vendor will conflict in
`vendor/`; resolve by re-vendoring rather than merging hunks.

`jellis206/fork-sync` automatically discovers non-archived forks, including this
one. It merges upstream changes daily or opens a `sync/upstream` conflict PR.
No per-repository registration is required.
