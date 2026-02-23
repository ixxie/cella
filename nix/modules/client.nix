{
  config,
  lib,
  pkgs,
  ...
}:
with lib; let
  cfg = config.cella.client;
  registryDir = "${config.users.users.${cfg.user}.home}/.config/cella";

in {
  options.cella.client = {
    enable = mkEnableOption "cella client (manages server registry and local DNS)";

    user = mkOption {
      type = types.str;
      description = "User to install cella config for";
    };

    servers = mkOption {
      type = types.attrsOf types.str;
      default = {};
      description = "Server registry (name = SSH target, e.g. grove = \"root@1.2.3.4\")";
      example = {
        grove = "root@95.216.229.121";
      };
    };

    vmConfig = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Cell base config directory for localhost (contains flake.nix exporting nixosModule)";
    };

    sync = mkOption {
      type = types.listOf types.str;
      default = [];
      description = "Files/directories to sync into remote cells (e.g. ~/.claude.json)";
      example = ["~/.claude.json" "~/.claude"];
    };
  };

  config = mkIf cfg.enable {
    # Write server registry and deploy localhost vm-config
    system.activationScripts.cella-client = let
      registry = concatStringsSep "\n" (mapAttrsToList (name: target: ''
        [${name}]
        target = "${target}"
      '') cfg.servers);
    in ''
      mkdir -p "${registryDir}"
      chown ${cfg.user}: "${registryDir}"
      cat > "${registryDir}/servers.toml" << 'REGISTRY'
      ${registry}
      REGISTRY
      chown ${cfg.user}: "${registryDir}/servers.toml"

      ${optionalString (cfg.vmConfig != null) ''
        rm -rf /var/lib/cella/vm-config
        cp -rL "${cfg.vmConfig}" /var/lib/cella/vm-config
        chmod -R a+rX /var/lib/cella/vm-config
      ''}

      ${optionalString (cfg.sync != []) (let
        syncToml = concatStringsSep ", " (map (s: ''"${s}"'') cfg.sync);
      in ''
        cat > "${registryDir}/config.toml" << 'CLIENTCFG'
        sync = [${syncToml}]
        CLIENTCFG
        chown ${cfg.user}: "${registryDir}/config.toml"
      '')}
    '';

    # Make /etc/hosts writable so `cella tunnel` can add .cell entries at runtime.
    # NixOS normally symlinks this to the nix store (read-only).
    # Setting a mode copies it instead, making it mutable until the next rebuild.
    environment.etc.hosts.mode = "0644";

    # Ensure runtime dir exists
    systemd.tmpfiles.rules = [
      "d /run/cella 0755 root root -"
    ];

    # Sudo rules for tunnel management
    security.sudo.extraRules = [
      {
        users = [cfg.user];
        commands = [
          { command = "${pkgs.iproute2}/bin/ip addr add 127.* dev lo"; options = ["NOPASSWD"]; }
          { command = "${pkgs.iproute2}/bin/ip addr del 127.* dev lo"; options = ["NOPASSWD"]; }
          { command = "${pkgs.cella}/bin/cella hosts *"; options = ["NOPASSWD"]; }
        ];
      }
    ];
  };
}
