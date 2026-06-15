# Changelog

All notable changes to Mole are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project aims to
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- Continuous integration (cargo and Nix) on every push and pull request.
- `CONTRIBUTING.md`, a pull-request template, a `LICENSE` file, and this
  changelog.

### Changed
- A triggered hint now preempts the background pre-warm: it kills the in-flight
  background scan so a hint never waits on it.

## [0.1.0] - 2026-06-15

Initial release.

### Added
- Vimium-style two-letter hints over every line of on-screen text.
- Teleport, left/right/double click, drag-and-copy, and free `hjkl` movement.
- Background daemon with a thin client command, triggered from any key binding.
- TOML configuration with hot reload.
- Incremental scan cache with background pre-warming for near-instant hints.

[Unreleased]: https://github.com/lucasly-ba/mole/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/lucasly-ba/mole/releases/tag/v0.1.0
