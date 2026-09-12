{
  description = "AI coding TUI for chivalrous people";

  inputs = {
    nixpkgs.url = "git+https://github.com/NixOS/nixpkgs?ref=nixos-unstable";
    selune = {
      url = "git+https://github.com/chardoncs/selune?ref=main";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      selune,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      version =
        (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version
        + "+${self.shortRev or self.dirtyShortRev or "unknown"}";
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        rec {
          shuvarie = pkgs.rustPlatform.buildRustPackage {
            pname = "shuvarie";
            inherit version;

            src = self;

            cargoLock.lockFile = ./Cargo.lock;

            cargoBuildFlags = [
              "--bin"
              "shuvarie"
            ];

            postUnpack = ''
              rm -rf "$sourceRoot/vendor/selune"
              mkdir -p "$sourceRoot/vendor"
              cp -rL ${selune} "$sourceRoot/vendor/selune"
            '';
            meta = {
              description = "AI coding TUI for chivalrous people";
              homepage = "https://github.com/shuvarie/shuvarie";
              license = pkgs.lib.licenses.mit;
              mainProgram = "shuvarie";
              platforms = systems;
            };
          };

          default = shuvarie;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.shuvarie}/bin/shuvarie";
        };
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            packages = with pkgs; [
              cargo
              clippy
              git
              rust-analyzer
              rustc
              rustfmt
            ];
            env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
