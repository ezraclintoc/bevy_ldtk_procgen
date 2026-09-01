{
  description = "bevy_ldtk_procgen - procedural LDtk dungeon generation for Bevy";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = nixpkgs.legacyPackages.${system};
        inherit (pkgs) lib;

        buildInputs = with pkgs; [
          alsa-lib
          udev
          libxkbcommon
          wayland
          libx11
          libxcursor
          libxrandr
          libxi
        ];

        runtimeLibs = with pkgs; [
          vulkan-loader
          libGL
          libxkbcommon
          wayland
          alsa-lib
          udev
          libx11
          libxcursor
          libxrandr
          libxi
        ];

        libraryPath = lib.makeLibraryPath runtimeLibs;

        src = lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            let
              base = baseNameOf path;
            in
            !(builtins.elem base [
              "target"
              "result"
              ".direnv"
            ]);
        };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "bevy_ldtk_procgen";
          version = "0.1.0";
          inherit src buildInputs;

          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = with pkgs; [
            pkg-config
            makeWrapper
          ];

          cargoBuildFlags = [
            "--example"
            "dungeon"
          ];

          postInstall = ''
            install -Dm755 "target/${pkgs.stdenv.hostPlatform.rust.cargoShortTarget}/release/examples/dungeon" \
              "$out/bin/dungeon"
            mkdir -p "$out/share/bevy_ldtk_procgen"
            cp -r assets "$out/share/bevy_ldtk_procgen/assets"
            wrapProgram "$out/bin/dungeon" \
              --set BEVY_ASSET_ROOT "$out/share/bevy_ldtk_procgen" \
              --prefix LD_LIBRARY_PATH : "${libraryPath}"
          '';

          meta = {
            description = "Procedural LDtk dungeon generation for Bevy";
            license = with lib.licenses; [
              mit
              asl20
            ];
            mainProgram = "dungeon";
          };
        };

        apps.default = flake-utils.lib.mkApp { drv = self.packages.${system}.default; };

        devShells.default = pkgs.mkShell {
          inherit buildInputs;

          nativeBuildInputs = with pkgs; [
            cargo
            rustc
            rust-analyzer
            clippy
            rustfmt
            pkg-config
            prek
            typos
          ];

          env = {
            LD_LIBRARY_PATH = libraryPath;
            RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        };

        formatter = pkgs.nixfmt-tree;
      }
    );
}
