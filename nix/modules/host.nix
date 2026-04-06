{
  config,
  lib,
  pkgs,
  inputs,
  ...
}:
with lib; let
  cfg = config.cella.server;

  hostConfig = builtins.toJSON {
    bridge = {
      inherit (cfg.bridge) name address subnet;
    };
    proxy = {
      inherit (cfg.proxy) httpPort gitCredentialPort controlPort logFile;
    };
    user = {
      inherit (cfg.user) name uid authorizedKeys;
    };
    vm = {
      inherit (cfg.vm) vcpu mem varSize;
    };
  };

  proxyConfig = builtins.toJSON {
    cells = [];
    egress = {
      reads = {
        methods = cfg.egress.reads.methods;
        allowed = cfg.egress.reads.allowed;
        denied = cfg.egress.reads.denied;
      };
      writes = {
        methods = cfg.egress.writes.methods;
        allowed = cfg.egress.writes.allowed;
        denied = cfg.egress.writes.denied;
      };
      credentials = cfg.egress.credentials;
      passthrough = cfg.egress.passthrough;
    };
    httpPort = cfg.proxy.httpPort;
    gitCredentialPort = cfg.proxy.gitCredentialPort;
    controlPort = cfg.proxy.controlPort;
    logFile = cfg.proxy.logFile;
    bindAddress = cfg.bridge.address;
    sweepTimeout = cfg.sweep.timeout;
    sweepInterval = cfg.sweep.interval;
  };
in {
  imports = [
    ./options.nix
  ];

  config = mkIf cfg.enable {
    systemd.network.wait-online.enable = false;

    # Bridge network
    systemd.network = {
      enable = true;
      netdevs."10-cellabr" = {
        netdevConfig = {
          Name = cfg.bridge.name;
          Kind = "bridge";
        };
      };
      networks."10-cellabr" = {
        matchConfig.Name = cfg.bridge.name;
        networkConfig = {
          Address = "${cfg.bridge.address}/24";
          DHCPServer = false;
          ConfigureWithoutCarrier = true;
          DNS = [cfg.bridge.address];
          Domains = ["~cell"];
        };
        linkConfig.RequiredForOnline = "no";
      };
      # Attach VM tap devices to bridge
      networks."11-microvm" = {
        matchConfig.Name = "vm-*";
        networkConfig.Bridge = cfg.bridge.name;
      };
    };

    # NAT for proxy outbound
    networking.nat = {
      enable = true;
      internalInterfaces = [cfg.bridge.name];
      externalInterface = lib.mkIf (cfg.nat.interface != "auto") cfg.nat.interface;
    };

    boot.kernel.sysctl."net.ipv4.ip_forward" = 1;

    # nftables: cells can ONLY reach the proxy
    networking.nftables = {
      enable = true;
      tables.cella = {
        family = "inet";
        content = ''
          chain forward {
            type filter hook forward priority 0; policy drop;

            ct state established,related accept

            # cells -> proxy HTTP
            iifname "${cfg.bridge.name}" ip daddr ${cfg.bridge.address} tcp dport ${toString cfg.proxy.httpPort} accept
            # cells -> proxy git-credential
            iifname "${cfg.bridge.name}" ip daddr ${cfg.bridge.address} tcp dport ${toString cfg.proxy.gitCredentialPort} accept
            # cells -> control API (for flow rule updates)
            iifname "${cfg.bridge.name}" ip daddr ${cfg.bridge.address} tcp dport ${toString cfg.proxy.controlPort} accept
            # cells -> host SSH
            iifname "${cfg.bridge.name}" ip daddr ${cfg.bridge.address} tcp dport 22 accept

            # host -> cells (host is trusted)
            ip saddr ${cfg.bridge.address} oifname "${cfg.bridge.name}" accept

            # DROP everything else from cells
            iifname "${cfg.bridge.name}" drop

            # proxy (host) -> internet
            ${if cfg.nat.interface == "auto"
              then "oifname != \"${cfg.bridge.name}\" accept"
              else "oifname \"${cfg.nat.interface}\" accept"}
          }

          chain input {
            type filter hook input priority 0; policy accept;
            iifname "${cfg.bridge.name}" ip daddr ${cfg.bridge.address} tcp dport { ${toString cfg.proxy.httpPort}, ${toString cfg.proxy.gitCredentialPort}, ${toString cfg.proxy.controlPort} } accept
          }
        '';
      };
    };

    networking.firewall.trustedInterfaces = [cfg.bridge.name];

    # DNS for VMs — branch names resolve to VM IPs
    services.dnsmasq = {
      enable = true;
      settings = {
        interface = cfg.bridge.name;
        bind-interfaces = true;
        listen-address = cfg.bridge.address;
        server = ["1.1.1.1" "1.0.0.1"];
        no-resolv = true;
        no-dhcp-interface = cfg.bridge.name;
        addn-hosts = "/var/lib/cella/dns-hosts";
      };
    };

    # Generate a host SSH key for server-side VM access.
    # The public key is included in the host config so VMs authorize it.
    systemd.services.cella-hostkey = {
      description = "Generate cella host SSH key";
      wantedBy = ["multi-user.target"];
      before = ["cella-services.service"];
      serviceConfig.Type = "oneshot";
      serviceConfig.RemainAfterExit = true;
      script = ''
        KEY=/var/lib/cella/ssh/id_ed25519
        if [ ! -f "$KEY" ]; then
          mkdir -p /var/lib/cella/ssh
          ${pkgs.openssh}/bin/ssh-keygen -t ed25519 -f "$KEY" -N "" -C "cella-host"
        fi
        # symlink into root's .ssh so all server-side SSH commands use it
        mkdir -p /root/.ssh
        ln -sf "$KEY" /root/.ssh/id_ed25519
        ln -sf "$KEY.pub" /root/.ssh/id_ed25519.pub
      '';
    };

    systemd.tmpfiles.rules = [
      "d /var/lib/cella 0755 root root -"
      "d /var/lib/cella/ca 0755 root root -"
      "d /var/lib/cella/cells 0755 root root -"
      "d /var/lib/cella/copyfiles 0755 root root -"
      "d /var/log/cella 0755 root root -"
      "f /var/lib/cella/dns-hosts 0666 root root -"
      "f /var/lib/cella/ip-pool.json 0644 root root -"
    ];

    # Host config JSON — read by mkCell at nix eval time
    environment.etc."cella/host-config.json".text = hostConfig;

    # Allow git operations on cell repos (owned by uid 1000, accessed by root via SSH/cella-services)
    programs.git.enable = true;
    programs.git.config.safe.directory = "*";


    # Proxy config
    environment.etc."cella/proxy-config.json".text = proxyConfig;

    # Stage copyFiles into the shared directory
    systemd.services.cella-copyfiles = mkIf (cfg.vm.copyFiles != {}) {
      description = "Stage files for cella VMs";
      wantedBy = ["multi-user.target"];
      serviceConfig.Type = "oneshot";
      script = let
        copies = lib.concatStringsSep "\n" (lib.mapAttrsToList (src: dst: ''
            mkdir -p "$(dirname "/var/lib/cella/copyfiles/${dst}")"
            cp -f "${src}" "/var/lib/cella/copyfiles/${dst}"
          '')
          cfg.vm.copyFiles);
      in
        copies;
    };

    # MITM proxy (mitmproxy in regular mode — redsocks in guest handles transparency)
    systemd.services.cella-mitmproxy = {
      description = "Cella MITM Proxy";
      wantedBy = ["multi-user.target"];
      after = ["network.target"];
      serviceConfig = {
        ExecStart = "${pkgs.mitmproxy}/bin/mitmdump --listen-host ${cfg.bridge.address} --listen-port ${toString cfg.proxy.httpPort} --set confdir=/var/lib/cella/ca -s ${./proxy/cella_addon.py}";
        Restart = "always";
        RestartSec = 5;
        EnvironmentFile =
          (lib.optional (cfg.credentialsFile != null) cfg.credentialsFile)
          ++ ["-/var/lib/cella/secrets.env"];
        ReadWritePaths = ["/var/log/cella" "/var/lib/cella/ca" "/var/lib/cella"];
      };
    };

    # Sync the public CA cert from mitmproxy's combined key+cert file so
    # guests always trust the same key mitmproxy actually signs with
    systemd.services.cella-ca-sync = {
      description = "Sync mitmproxy CA public cert";
      wantedBy = ["multi-user.target"];
      after = ["cella-mitmproxy.service"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
      };
      script = ''
        CA=/var/lib/cella/ca/mitmproxy-ca.pem
        for i in $(seq 1 30); do
          [ -f "$CA" ] && break
          sleep 1
        done
        [ -f "$CA" ] || exit 1
        ${pkgs.openssl}/bin/openssl x509 -in "$CA" -out /var/lib/cella/ca/mitmproxy-ca-cert.pem
      '';
    };

    # Cella services (git credentials + control API)
    systemd.services.cella-services = {
      description = "Cella Git Credentials + Control API";
      wantedBy = ["multi-user.target"];
      after = ["network.target"];
      path = [pkgs.git pkgs.sudo pkgs.util-linux pkgs.systemd pkgs.openssh pkgs.curl pkgs.nix];
      serviceConfig = {
        ExecStart = "${pkgs.cella}/bin/cella server proxy --config /etc/cella/proxy-config.json";
        Restart = "always";
        RestartSec = 5;
        EnvironmentFile =
          (lib.optional (cfg.credentialsFile != null) cfg.credentialsFile)
          ++ ["-/var/lib/cella/secrets.env"];
      };
    };

    # Scoped sudo for VM management
    security.sudo.extraRules = [
      {
        users = ["root"];
        commands = [
          {
            command = "/run/current-system/sw/bin/systemctl start microvm@*";
            options = ["NOPASSWD"];
          }
          {
            command = "/run/current-system/sw/bin/systemctl stop microvm@*";
            options = ["NOPASSWD"];
          }
        ];
      }
    ];
  };
}
