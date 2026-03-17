{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let pkgs = nixpkgs.legacyPackages.${system}; in {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "zinc";
          version = "0.2.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [ pkgs.installShellFiles ];
          postInstall = ''
            installShellCompletion --bash target/*/build/zinc-cli-*/out/completions/zinc.bash
            installShellCompletion --zsh --name _zinc target/*/build/zinc-cli-*/out/completions/_zinc
            installShellCompletion --fish target/*/build/zinc-cli-*/out/completions/zinc.fish
            installManPage target/*/build/zinc-cli-*/out/man/zinc.1
          '';
        };
      }
    );
}
