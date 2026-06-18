{
  description = "RMUX — a local terminal multiplexer with a tmux-style CLI, daemon runtime, Rust SDK, and ratatui integration.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;

      # Single source of truth for the version: parse it out of the workspace
      # manifest's [workspace.package] table so the number is only ever edited in
      # Cargo.toml. On release, bump the version there and the flake follows.
      cargoToml = builtins.readFile ./Cargo.toml;
      version =
        let
          # Match the first `version = "x.y.z"` after the [workspace.package] header.
          after = nixpkgs.lib.last (nixpkgs.lib.splitString "[workspace.package]" cargoToml);
          m = builtins.match ".*[\n\r]+version[[:space:]]*=[[:space:]]*\"([^\"]+)\".*" after;
        in
        if m == null
        then throw "flake.nix: could not parse version from [workspace.package] in Cargo.toml"
        else builtins.head m;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "rmux";
            inherit version;

            src = ./.;

            # Use the committed Cargo.lock as the source of dependency hashes.
            # This keeps builds reproducible without a manually maintained
            # cargoHash that would need recomputing on every dependency change.
            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            # Tests are not run during the Nix build. The tests/ integration
            # suites spawn the daemon over real PTYs/sockets, shell out to host
            # tools like `ps`, and some are timing-sensitive (e.g. interactive
            # choose-tree redraw races) — unreliable in the hermetic,
            # CPU-saturated build sandbox even though they pass under
            # `cargo test`. The full suite runs in the project's CI and locally.
            # See docs/nix-packaging.md.
            doCheck = false;

            # The workspace defaults to publish = false for library crates and
            # builds both the `rmux` and `rmux-daemon` binaries from the root crate.

            meta = with pkgs.lib; {
              description = "A local terminal multiplexer with a tmux-style CLI, daemon runtime, Rust SDK, and ratatui integration.";
              homepage = "https://github.com/helvesec/rmux";
              license = with licenses; [ mit asl20 ]; # MIT OR Apache-2.0
              mainProgram = "rmux";
            };
          };
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/rmux";
        };
      });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            buildInputs = with pkgs; [
              cargo
              rustc
              rust-analyzer
              rustfmt
              clippy
            ];

            RUST_SRC_PATH = "${pkgs.rust.packages.stable.rustPlatform.rustLibSrc}";
          };
        }
      );
    };
}
