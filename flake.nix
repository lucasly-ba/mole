{
  description = "mole — keyboard-only mouse navigation for Linux/X11 (Vimium-style hints via OCR)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { nixpkgs, flake-utils, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };

        # Build-time native dependencies (pkg-config-discovered C libraries).
        # makeWrapper lets us put the runtime tools on the installed binary's PATH.
        nativeBuildInputs = [ pkgs.pkg-config pkgs.makeWrapper ];

        # Link-time native dependencies. cairo is the only hard one; the X11
        # stack used at runtime is reached through x11rb's pure-Rust backend.
        buildInputs = [
          pkgs.cairo
          pkgs.glib
        ];

        # Runtime tools that are looked up by name (not linked). Tesseract is the
        # OCR engine mole shells out to (a hard runtime requirement); the X
        # libraries help when running on a real display. None of these are
        # required to *build* the crate.
        runtimeTools = [
          pkgs.tesseract
          pkgs.libx11
          pkgs.libxcb
        ];
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "mole";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          inherit nativeBuildInputs buildInputs;

          # The X11/cairo-surface code needs a live display; only the pure-logic
          # unit tests are meaningful in the sandboxed build.
          checkFlags = [
            "--skip=x11::"
            "--skip=overlay"
          ];

          # mole shells out to `tesseract` at runtime; put it on the installed
          # binary's PATH so `nix profile install` is self-contained (otherwise
          # the daemon fails with "failed to spawn tesseract" outside a dev shell).
          postFixup = ''
            wrapProgram $out/bin/mole \
              --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.tesseract ]}
          '';

          meta = with pkgs.lib; {
            description = "Keyboard-only mouse navigation for Linux/X11";
            homepage = "https://github.com/lucasly-ba/mole";
            license = licenses.mit;
            mainProgram = "mole";
            platforms = platforms.linux;
          };
        };

        devShells.default = pkgs.mkShell {
          inherit buildInputs;
          nativeBuildInputs = nativeBuildInputs ++ [
            pkgs.cargo
            pkgs.rustc
            pkgs.rust-analyzer
            pkgs.rustfmt
            pkgs.clippy
          ] ++ runtimeTools;

          RUST_LOG = "mole=debug";
          # Help any pure-Rust crate that still wants to dlopen the X libs.
          LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath buildInputs;

          # Inside the dev shell, `mole <cmd>` runs the local (debug) build so
          # you can iterate without reinstalling. End users get `mole` on PATH
          # via `nix profile install` instead (see the README).
          shellHook = ''
            mole() { cargo run --quiet -- "$@"; }
            echo "dev shell ready — 'mole daemon', 'mole click', … run the local build (cargo run)."
          '';
        };

        formatter = pkgs.nixpkgs-fmt;
      }
    );
}
