# Changelog

All notable changes to this project are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-08-30

First release: a Rust port of [bestline](https://github.com/jart/bestline),
with Emacs-style editing, reverse history search, completion, hints and UTF-8
editing over ANSI X3.64 escape sequences, and no terminfo dependency.

### Added

- `Rustline`, the line editor, with `readline`, `readline_with_init`,
  `readline_raw` and `read_password`.
- `Config` for hints, completion, multi-line entry, bracketed paste, paren
  balancing, bracket highlighting and mask mode.
- `History`, with load and save to a mode-`0600` file, and `history_path` /
  `readline_with_history` for the conventional `~/.{prog}_history`.
- `CompletionProvider` with `FileCompleter` and `CommandCompleter`, and
  `HintProvider`.
- `set_xlat`, for input methods that map one keyboard onto another script.

### Dependencies

- `nix`, `unicode-width` and `thiserror` only. `home_dir` is resolved from
  `$HOME` with a passwd fallback rather than through `dirs`, which reached an
  MPL-2.0 crate (`option-ext`) via `dirs-sys` and put an MPL entry in the
  licence audit of every crate depending on rustline.

[Unreleased]: https://github.com/matt-dunleavy/rustline/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/matt-dunleavy/rustline/releases/tag/v0.1.0
