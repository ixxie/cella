# cella.lib.mkHost — creates a NixOS configuration for a cella server.
#
# Usage in a host flake:
#   cella.lib.mkHost { inherit cella nixpkgs disko; } {
#     name = "myhost";
#     disk = ./disk.nix;
#     sshPubkey = "ssh-ed25519 ...";
#     config = { cella.server.enable = true; };
#   }
{ cella, nixpkgs, disko }:

{
  name,
  disk,
  sshPubkey,
  system ? "x86_64-linux",
  config ? {},
}: {
  nixosConfigurations.${name} = nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      disko.nixosModules.disko
      cella.nixosModules.server
      disk
      ({pkgs, ...}: {
        networking.hostName = name;
        system.stateVersion = "24.11";

        # virtio drivers for cloud/VM hosts
        imports = [(nixpkgs + "/nixos/modules/profiles/qemu-guest.nix")];

        # server networking
        networking.useDHCP = true;
        networking.firewall.allowedTCPPorts = [22];

        # essential packages
        environment.systemPackages = [
          cella.packages.${system}.default
          pkgs.git
        ];

        # SSH access
        services.openssh.enable = true;
        users.users.root.openssh.authorizedKeys.keys = [sshPubkey];
      })
      config
    ];
  };
}
