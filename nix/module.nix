{ pkgs, lib, config, ... }:

{
  options.services.filetracker-rs = {
    enable = lib.mkEnableOption "filetracker-rs server";

    package = lib.mkPackageOption pkgs "filetracker-rs" {
      default = [ "filetracker-rs" ];
    };

    listenAddress = lib.mkOption {
      default = "0.0.0.0";
      description = "The address that the filetracker server will listen on";
      type = lib.types.str;
    };

    port = lib.mkOption {
      default = 9999;
      description = "The port that the filetracker server will listen on";
      type = lib.types.port;
    };

    ensureFiles = lib.mkOption {
      default = { };
      description = "Files that should be added to filetracker after start";
      type = lib.types.attrsOf lib.types.path;
    };
  };

  config =
    let
      cfg = config.services.filetracker-rs;

      createEnsureService = remotePath: localPath:
        let
          systemdEscapedPath = builtins.replaceStrings [ "/" ] [ "-" ] (lib.removePrefix "/" remotePath);
          serviceName = "filetracker-put-${systemdEscapedPath}";
          python = pkgs.python3.withPackages (pp: [ pp.requests ]);
        in
        lib.nameValuePair serviceName {
          enable = true;
          description = "Filetracker ensure ${remotePath}";
          after = [ "filetracker.service" ];
          partOf = [ "filetracker.service" ];
          wantedBy = [ "filetracker.service" ];

          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = "true";
            ExecStart = ''
              ${python}/bin/python3 ${./filetracker-ensure.py} \
                http://127.0.0.1:${builtins.toString cfg.port} \
                ${lib.escapeShellArg remotePath} \
                ${lib.escapeShellArg localPath}
            '';
          };
        };
    in
    lib.mkIf cfg.enable {
      users.extraUsers.filetracker = {
        isSystemUser = true;
        group = "filetracker";
      };
      users.extraGroups.filetracker = { };

      systemd.services = {
        filetracker = {
          enable = true;
          description = "Filetracker Server";
          after = [ "network.target" ];
          wantedBy = [ "multi-user.target" ];

          script = ''
            exec ${cfg.package}/bin/filetracker-rs \
              -d /var/lib/filetracker \
              -l ${lib.escapeShellArg cfg.listenAddress}:${builtins.toString cfg.port}
          '';

          serviceConfig = {
            Type = "simple";
            StateDirectory = "filetracker";
            User = "filetracker";
            Group = "filetracker";

            PrivateTmp = true;
            ProtectSystem = "strict";
            RemoveIPC = true;
            NoNewPrivileges = true;
            RestrictSUIDSGID = true;
            ProtectKernelTunables = true;
            ProtectControlGroups = true;
            ProtectKernelModules = true;
            ProtectKernelLogs = true;
            PrivateDevices = true;

            Restart = "on-failure";
          };
        };
      } // (lib.mapAttrs' createEnsureService cfg.ensureFiles);
    };

}
