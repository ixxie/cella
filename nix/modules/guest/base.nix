{
  config,
  lib,
  pkgs,
  cellaHost,
  cell,
  ...
}: let
  inherit (cellaHost) bridge proxy user nucleus;
  workspace = "/${cell.repo}";
in {
  system.stateVersion = "24.11";

  # Networking
  systemd.network.enable = true;
  networking.useNetworkd = true;

  # System-wide proxy
  networking.proxy = {
    httpProxy = "http://${bridge.address}:${toString proxy.httpPort}";
    httpsProxy = "http://${bridge.address}:${toString proxy.httpPort}";
    noProxy = "localhost,127.0.0.1,${bridge.address}";
  };

  # Transparent proxy — redsocks catches apps that ignore proxy env vars
  services.redsocks = {
    enable = true;
    redsocks = [
      {
        port = 12345;
        proxy = "${bridge.address}:${toString proxy.httpPort}";
        type = "http-relay";
        redirectCondition = "--dport 80";
        doNotRedirect = [
          "-d 127.0.0.0/8"
          "-d ${bridge.address}"
        ];
      }
      {
        port = 12346;
        proxy = "${bridge.address}:${toString proxy.httpPort}";
        type = "http-connect";
        redirectCondition = "--dport 443";
        doNotRedirect = [
          "-d 127.0.0.0/8"
          "-d ${bridge.address}"
        ];
      }
    ];
  };

  # Egress firewall
  networking.firewall = {
    enable = true;
    allowedTCPPorts = [22];
    trustedInterfaces = ["enp+"];
    extraCommands = ''
      iptables -P OUTPUT DROP
      iptables -A OUTPUT -o lo -j ACCEPT
      iptables -A OUTPUT -m state --state ESTABLISHED,RELATED -j ACCEPT
      iptables -A OUTPUT -d ${bridge.address} -j ACCEPT

      # Expose localhost-bound services to the host via the bridge
      iptables -t nat -A PREROUTING ! -i lo -p tcp -j REDIRECT
    '';
    extraStopCommands = ''
      iptables -P OUTPUT ACCEPT
      iptables -F OUTPUT
    '';
  };

  # SSH
  services.openssh = {
    enable = true;
    hostKeys = [
      {
        path = "/var/ssh-keys/ssh_host_ed25519_key";
        type = "ed25519";
      }
    ];
    settings = {
      ClientAliveInterval = 30;
      ClientAliveCountMax = 3;
    };
  };

  systemd.services.ssh-key-setup = {
    description = "Copy SSH keys from cell mount";
    wantedBy = ["sshd.service"];
    before = ["sshd.service"];
    serviceConfig.Type = "oneshot";
    script = ''
      mkdir -p /var/ssh-keys
      if [ -f ${workspace}/keys/ssh_host_ed25519_key ]; then
        cp ${workspace}/keys/ssh_host_ed25519_key /var/ssh-keys/
        chmod 600 /var/ssh-keys/ssh_host_ed25519_key
      fi

      USER_HOME="/home/${user.name}"
      mkdir -p "$USER_HOME/.ssh"
      if [ -f ${workspace}/keys/authorized_keys ] && [ -s ${workspace}/keys/authorized_keys ]; then
        cp ${workspace}/keys/authorized_keys "$USER_HOME/.ssh/authorized_keys"
      fi
      chmod 700 "$USER_HOME/.ssh"
      chmod 600 "$USER_HOME/.ssh/authorized_keys" 2>/dev/null || true
      chown -R ${user.name}:users "$USER_HOME/.ssh"
    '';
  };

  # User — no sudo
  users.users.${user.name} = {
    isNormalUser = true;
    uid = user.uid;
    group = "users";
    home = "/home/${user.name}";
    initialHashedPassword = "";
    openssh.authorizedKeys.keys = user.authorizedKeys;
  };

  # Nucleus user — network-restricted review agent
  users.users.nucleus = lib.mkIf nucleus.enable {
    isSystemUser = true;
    group = "users";
    uid = 999;
  };

  security.sudo = {
    enable = nucleus.enable;
    extraRules = lib.mkIf nucleus.enable [
      {
        users = [user.name];
        commands = [
          {
            command = "/run/current-system/sw/bin/su -s /bin/sh nucleus -c *";
            options = ["NOPASSWD"];
          }
        ];
      }
    ];
  };

  services.getty.autologinUser = user.name;

  environment = {
    enableAllTerminfo = true;

    systemPackages = with pkgs; [
      git
      curl
      tmux
      jq
    ];

    variables = {
      SSL_CERT_FILE = "/etc/ssl/cella-ca-bundle.crt";
      NIX_SSL_CERT_FILE = "/etc/ssl/cella-ca-bundle.crt";
      CURL_CA_BUNDLE = "/etc/ssl/cella-ca-bundle.crt";
    };

    etc."gitconfig".text = ''
      [safe]
        directory = *
    '';

    etc."tmux.conf".text = ''
      set -g status off
      set -g mouse off
      set -g default-terminal "tmux-256color"
      set -ga terminal-overrides ",*:Tc"
      set -g prefix None
      bind -n C-] detach-client
    '';
  };

  # Kernel hardening
  boot.kernel.sysctl = {
    "kernel.dmesg_restrict" = 1;
    "kernel.sysrq" = 0;
    "kernel.yama.ptrace_scope" = 2;
    "kernel.kptr_restrict" = 2;
  };

  boot.tmp = {
    useTmpfs = true;
    tmpfsSize = "1G";
  };

  nix.settings.experimental-features = ["nix-command" "flakes"];

  # Trust the mitmproxy CA
  systemd.services.cella-ca-trust = {
    description = "Install mitmproxy CA certificate";
    wantedBy = ["multi-user.target"];
    after = ["local-fs.target"];
    before = ["nix-daemon.service" "redsocks.service"];
    serviceConfig.Type = "oneshot";
    script = ''
      BUNDLE=/etc/ssl/cella-ca-bundle.crt
      CA=/etc/cella/ca/mitmproxy-ca-cert.pem

      for i in $(seq 1 30); do
        [ -f "$CA" ] && break
        sleep 1
      done

      cp /etc/ssl/certs/ca-certificates.crt "$BUNDLE" 2>/dev/null || touch "$BUNDLE"
      if [ -f "$CA" ]; then
        cat "$CA" >> "$BUNDLE"
      fi
    '';
  };

  systemd.services.nix-daemon.environment = {
    NIX_SSL_CERT_FILE = lib.mkForce "/etc/ssl/cella-ca-bundle.crt";
    CURL_CA_BUNDLE = lib.mkForce "/etc/ssl/cella-ca-bundle.crt";
    SSL_CERT_FILE = "/etc/ssl/cella-ca-bundle.crt";
  };

  i18n.defaultLocale = "en_US.UTF-8";
  time.timeZone = "UTC";
}
