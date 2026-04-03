# cella.lib.mkCell — creates a NixOS microVM configuration for a cell.
#
# Used by the per-cell wrapper flake generated at runtime by `cella up`.
# The wrapper flake passes inputs and cell-specific parameters.
{ cella, nixpkgs, microvm }:

{
  name,
  ip,
  cellDir,
  repo ? "cell",
  hostConfig ? {},
  modules ? [],
  system ? "x86_64-linux",
}: let
  bridge = hostConfig.bridge or { address = "192.168.83.1"; name = "cellabr"; subnet = "192.168.83.0/24"; };
  proxy = hostConfig.proxy or { httpPort = 8080; gitCredentialPort = 8081; controlPort = 8082; logFile = "/var/log/cella/proxy.log"; };
  user = hostConfig.user or { name = "agent"; uid = 1000; authorizedKeys = []; };
  vm = hostConfig.vm or { vcpu = 4; mem = 4096; varSize = 4096; };

  cell = { inherit ip name cellDir repo; };
in {
  nixosConfigurations.${name} = nixpkgs.lib.nixosSystem {
    inherit system;
    specialArgs = {
      inherit cell;
      cellxPkg = cella.packages.${system}.cellx;
      cellaHost = { inherit bridge proxy user vm; };
    };
    modules = [
      microvm.nixosModules.microvm
      (cella + "/nix/modules/guest/base.nix")
      {
        networking.hostName = "cell";
        microvm = {
          vcpu = vm.vcpu;
          mem = vm.mem;
          hypervisor = "qemu";
          interfaces = [
            {
              type = "tap";
              id = "vm-${name}";
              mac = let
                hash = builtins.hashString "md5" name;
                b1 = builtins.substring 0 2 hash;
                b2 = builtins.substring 2 2 hash;
                b3 = builtins.substring 4 2 hash;
                b4 = builtins.substring 6 2 hash;
              in "02:ce:${b1}:${b2}:${b3}:${b4}";
            }
          ];
          volumes = [
            {
              image = "var.img";
              mountPoint = "/var";
              size = vm.varSize;
            }
          ];
          shares = [
            {
              tag = "ro-store";
              source = "/nix/store";
              mountPoint = "/nix/.ro-store";
              proto = "virtiofs";
            }
            {
              tag = "cell";
              source = "${cellDir}/repo";
              mountPoint = "/${repo}";
              proto = "virtiofs";
            }
            {
              tag = "cella-ca";
              source = "/var/lib/cella/ca";
              mountPoint = "/etc/cella/ca";
              proto = "virtiofs";
            }
            {
              tag = "cella-copyfiles";
              source = "/var/lib/cella/copyfiles";
              mountPoint = "/etc/cella/copyfiles";
              proto = "virtiofs";
            }
          ];
          writableStoreOverlay = "/nix/.rw-store";
        };
        systemd.network.networks."10-lan" = {
          matchConfig.Type = "ether";
          networkConfig = {
            Address = "${ip}/24";
            Gateway = bridge.address;
            DNS = bridge.address;
          };
        };
      }
    ]
    ++ modules;
  };
}
