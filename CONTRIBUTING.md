# Contributing to Mole

Thanks for your interest! Mole is a small, focused codebase and contributions are
welcome: bug fixes, new backends, docs, anything.

Read [JOURNEY.md](JOURNEY.md) first: it explains how the pipeline is shaped and
*why*, which makes most changes obvious where they belong.

## Getting the source

Clone the repository.

**Via SSH (recommended)** if you plan to contribute regularly. This needs your
[SSH keys on GitHub](https://docs.github.com/en/authentication/connecting-to-github-with-ssh):

```sh
git clone git@github.com:lucasly-ba/mole.git
cd mole
```

**Via HTTPS** for a read-only checkout, or if you haven't set up SSH:

```sh
git clone https://github.com/lucasly-ba/mole.git
cd mole
```

## Setting up your fork

To submit changes you push to your own fork and open a pull request from it.

Fork `lucasly-ba/mole` with the **Fork** button (top-right of the GitHub page).
This creates `<username>/mole`. Add it as a remote:

```sh
# Using 'my-remote' as a placeholder name.

# If you cloned over SSH:
git remote add my-remote git@github.com:<username>/mole.git

# If you cloned over HTTPS:
git remote add my-remote https://github.com/<username>/mole.git
```

Then create a branch and push it to your fork:

```sh
# Using 'my-branch' as a placeholder name.
git checkout -b my-branch
git push --set-upstream my-remote my-branch
```

From there, open a pull request against `lucasly-ba/mole`'s `main` branch.

## Building and testing

Mole is a normal Cargo project. You need the **cairo** library to build and the
**tesseract** binary at runtime (both are in every distro's repositories).

```sh
cargo build
cargo test                       # pure-logic suite; no display needed
cargo fmt --all                  # format
cargo clippy --all-targets -- -D warnings
```

### Nix

A flake is provided for a reproducible toolchain (cairo and tesseract wired in):

```sh
nix develop -c cargo test
nix build                        # packaged binary, runs the sandboxed tests
```

> The flake only sees git-tracked files, so `git add` a new file before
> `nix develop` / `nix build`, or Nix reports it as "not tracked by Git".

## What CI checks

Every push and pull request runs [`.github/workflows/ci.yml`](.github/workflows/ci.yml):

- a plain-cargo job: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`;
- a Nix job: `nix build` and `nix flake check`.

Run `cargo fmt` and `cargo clippy` locally before pushing and you'll match it.

## Testing philosophy

Pure logic is unit-tested; the seams between modules are covered by
`tests/pipeline.rs`. The X11, OCR-subprocess, overlay and clipboard paths touch a
live display or another process, so they're verified on a real X session rather
than in the unit suite (the flake and CI skip them headlessly; see JOURNEY §5).
If you add pure logic, add tests for it; if you touch the display paths, say in
the PR how you verified them.

## Pull requests

- Branch off `main`, keep changes focused, and prefer several small commits over
  one large one.
- Make sure `cargo fmt`, `cargo clippy` and the tests pass.
- Update the docs (README / JOURNEY) when behaviour changes.
- Open the PR against `main`; CI must be green before it can merge.

By contributing, you agree your work is licensed under the project's
[MIT License](LICENSE).
