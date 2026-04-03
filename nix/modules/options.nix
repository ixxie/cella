{lib, ...}:
with lib; {
  options.cella.server = {
    enable = mkEnableOption "Cella server (sandboxed microVMs)";

    nat.interface = mkOption {
      type = types.str;
      default = "auto";
      description = "External network interface for NAT. Use \"auto\" to masquerade on all interfaces.";
    };

    bridge = {
      name = mkOption {
        type = types.str;
        default = "cellabr";
      };
      address = mkOption {
        type = types.str;
        default = "192.168.83.1";
      };
      subnet = mkOption {
        type = types.str;
        default = "192.168.83.0/24";
      };
    };

    proxy = {
      httpPort = mkOption {
        type = types.port;
        default = 8080;
      };
      gitCredentialPort = mkOption {
        type = types.port;
        default = 8081;
      };
      logFile = mkOption {
        type = types.str;
        default = "/var/log/cella/proxy.log";
      };
      controlPort = mkOption {
        type = types.port;
        default = 8082;
        description = "Control API port (localhost only)";
      };
    };

    credentialsFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = "Environment file with API credentials (KEY=value per line)";
    };

    egress = {
      reads = {
        methods = mkOption {
          type = types.listOf types.str;
          default = ["GET" "HEAD" "OPTIONS"];
          description = "HTTP methods classified as reads";
        };
        allowed = mkOption {
          type = types.either types.str (types.listOf types.str);
          default = "*";
          description = "Allowed domains for read methods. Use \"*\" for all.";
        };
        denied = mkOption {
          type = types.listOf types.str;
          default = [];
          description = "Denied domains for read methods (overrides allowed)";
        };
      };
      writes = {
        methods = mkOption {
          type = types.listOf types.str;
          default = ["POST" "PUT" "PATCH" "DELETE"];
          description = "HTTP methods classified as writes";
        };
        allowed = mkOption {
          type = types.listOf types.str;
          default = [
            "github.com"
            "*.github.com"
            "*.githubusercontent.com"
            "registry.npmjs.org"
            "*.npmjs.org"
            "pypi.org"
            "*.pypi.org"
            "files.pythonhosted.org"
            "cache.nixos.org"
            "*.cachix.org"
          ];
          description = "Allowed domains for write methods";
        };
        denied = mkOption {
          type = types.either types.str (types.listOf types.str);
          default = "*";
          description = "Denied domains for write methods. Use \"*\" for all.";
        };
      };
      passthrough = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Domains to pass through without TLS interception (e.g. for OAuth)";
        example = ["claude.ai" "*.anthropic.com"];
      };
      credentials = mkOption {
        type = types.listOf (types.submodule {
          options = {
            host = mkOption {type = types.str;};
            header = mkOption {type = types.str;};
            envVar = mkOption {type = types.str;};
          };
        });
        default = [
          {
            host = "api.github.com";
            header = "Authorization";
            envVar = "GITHUB_TOKEN_HEADER";
          }
        ];
      };
    };

    vm = {
      vcpu = mkOption {
        type = types.int;
        default = 4;
      };
      mem = mkOption {
        type = types.int;
        default = 4096;
      };
      varSize = mkOption {
        type = types.int;
        default = 4096;
      };
      copyFiles = mkOption {
        type = types.attrsOf types.str;
        default = {};
        description = "Host files to copy into guest on boot (key = host path, value = guest path)";
      };
      mounts = mkOption {
        type = types.attrsOf (types.submodule {
          options = {
            mountPoint = mkOption {type = types.str;};
            readOnly = mkOption {
              type = types.bool;
              default = false;
            };
          };
        });
        default = {};
        description = "Host paths to mount into vm VMs (key = source path)";
      };
    };

    user = {
      name = mkOption {
        type = types.str;
        default = "agent";
      };
      uid = mkOption {
        type = types.int;
        default = 1000;
      };
      authorizedKeys = mkOption {
        type = types.listOf types.str;
        default = [];
      };
    };
  };
}
