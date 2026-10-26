# Declarative NixOS module: services.tuxedo-control
#
#   # Use a built-in profile (clean default):
#   services.tuxedo-control = { enable = true; defaultProfile = "Quiet"; };
#
#   # Or declare a custom profile from the config:
#   services.tuxedo-control = {
#     enable = true;
#     performanceProfile = "power_save";
#     fan.curve = [ { temp = 25; speed = 0; } { temp = 60; speed = 30; } { temp = 90; speed = 100; } ];
#     keyboard.backlight.brightness = 2;   # 0..max_brightness (0–4 on the reference board)
#     charging.profile = "stationary";     # stationary | balanced | high_capacity
#   };
self: { config, lib, pkgs, ... }:
let
  cfg = config.services.tuxedo-control;
  json = builtins.toJSON (
    {
      poll_seconds = cfg.pollSeconds;
      hysteresis_c = cfg.hysteresisC;
      profile = cfg.defaultProfile;
    }
    # Only emit a custom curve/perf (which creates a "Configured" profile) when set;
    # otherwise the daemon uses its built-in profiles and selects `defaultProfile`.
    // lib.optionalAttrs (cfg.performanceProfile != null) { perf_profile = cfg.performanceProfile; }
    // lib.optionalAttrs (cfg.fan.curve != [ ]) { curve = map (p: [ p.temp p.speed ]) cfg.fan.curve; }
    // lib.optionalAttrs (cfg.keyboard.backlight.brightness != null) { kbd_brightness = cfg.keyboard.backlight.brightness; }
    // lib.optionalAttrs (cfg.charging.profile != null) { charge_profile = cfg.charging.profile; }
  );
  configFile = pkgs.writeText "tuxedo-control.json" json;
in
{
  options.services.tuxedo-control = {
    enable = lib.mkEnableOption "declarative TUXEDO Uniwill fan/performance control (tuxedo-control)";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.system}.default;
      defaultText = lib.literalExpression "tuxedo-control.packages.\${system}.default";
      description = "The tuxedo-control package providing tuxedo-controld.";
    };

    defaultProfile = lib.mkOption {
      type = lib.types.str;
      default = "Quiet";
      example = "High Performance";
      description = ''
        Name of the profile to activate at startup. Built-ins:
        "Max Energy Save", "Quiet", "Office", "High Performance". If you set
        {option}`performanceProfile`/{option}`fan.curve`, a "Configured" profile is created
        and you may name it here instead.
      '';
    };

    pollSeconds = lib.mkOption {
      type = lib.types.ints.positive;
      default = 2;
      description = "How often to sample temperature and re-assert the fan duty.";
    };

    hysteresisC = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 3;
      description = "Cooling hysteresis in °C; the fan ramps down only once it is this much cooler.";
    };

    performanceProfile = lib.mkOption {
      type = lib.types.nullOr (lib.types.enum [ "power_save" "enthusiast" "overboost" ]);
      default = null;
      example = "power_save";
      description = ''
        Optional: create a "Configured" profile using this TUXEDO performance mode. Leave
        `null` to use the built-in profiles selected by {option}`defaultProfile`.
      '';
    };

    fan.curve = lib.mkOption {
      type = lib.types.listOf (lib.types.submodule {
        options = {
          temp = lib.mkOption { type = lib.types.ints.unsigned; description = "Temperature °C."; };
          speed = lib.mkOption { type = lib.types.ints.between 0 100; description = "Fan duty %."; };
        };
      });
      default = [ ];
      example = lib.literalExpression ''[ { temp = 25; speed = 0; } { temp = 90; speed = 100; } ]'';
      description = ''
        Optional ascending (temp °C → fan duty %) points for the "Configured" profile. Empty
        uses the built-in profiles. Note the driver enforces a ~25% on-speed floor; duties
        below ~12% become 0 (fan off).
      '';
    };

    keyboard.backlight.brightness = lib.mkOption {
      # Bounded (not just unsigned) so a typo fails at eval time rather than overflowing the
      # daemon's i32 config field, which would make the whole config parse fall back to defaults.
      type = lib.types.nullOr (lib.types.ints.between 0 255);
      default = null;
      example = 2;
      description = ''
        Optional keyboard-backlight brightness level, re-asserted on each daemon start.
        The range is hardware-defined (0 = off up to the LED's `max_brightness`; the
        reference board exposes 0–4). Values above the maximum are clamped by the driver.
        Leave `null` to not manage the backlight. Applies only if the laptop has a
        controllable backlight LED.
      '';
    };

    charging.profile = lib.mkOption {
      type = lib.types.nullOr (lib.types.enum [ "stationary" "balanced" "high_capacity" ]);
      default = null;
      example = "stationary";
      description = ''
        Optional battery charging profile, re-asserted on each daemon start:
        "stationary" (~60% cap), "balanced" (~80%), or "high_capacity" (100%). Leave
        `null` to not manage charging. Applies only if the EC exposes charging profiles.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    # The daemon needs the tuxedo_io char device from the tuxedo-drivers kernel modules.
    hardware.tuxedo-drivers.enable = lib.mkDefault true;
    boot.kernelModules = [ "tuxedo_io" ];

    # This module replaces tuxedo-rs/tailord. Running both fights over the same EC fan
    # registers. mkForce because a stale tuxedo-rs.enable=true would break fan control.
    hardware.tuxedo-rs.enable = lib.mkForce false;

    systemd.services.tuxedo-control = {
      description = "TUXEDO Uniwill fan/performance control (tuxedo-control)";
      wantedBy = [ "multi-user.target" ];
      after = [ "systemd-modules-load.service" ];
      # Retry while uniwill_wmi comes up at boot, then give up instead of restarting forever
      # (with RestartSec=3 the default 5-in-10s limit never trips).
      startLimitIntervalSec = 120;
      startLimitBurst = 10;
      serviceConfig = {
        # getExe' picks the explicit binary; getExe would resolve mainProgram (the GUI), not the daemon.
        ExecStart = "${lib.getExe' cfg.package "tuxedo-controld"} ${configFile}";
        Restart = "on-failure";
        RestartSec = 3;
        StateDirectory = "tuxedo-control"; # /var/lib/tuxedo-control for the profiles store
        # restore_auto() runs on SIGTERM in the daemon, so a clean stop hands the fan to the EC.
        # The first fan write after start takes ~13 s (EC fan-table init); don't SIGKILL a stop
        # that lands inside it, and don't let a hung stop hold up suspend for the default 90 s.
        TimeoutStopSec = 30;
        Nice = -5;
      };
    };

    # Hibernation resets the EC (profile lost, EC takes the fan back). Stop the daemon across
    # sleep and start it on resume so its EC setup runs again. Same pattern as tccd-sleep.
    systemd.services.tuxedo-control-sleep = {
      description = "Stop tuxedo-controld across sleep and start it again on resume";
      wantedBy = [ "sleep.target" ];
      before = [ "sleep.target" ];
      unitConfig.StopWhenUnneeded = true;
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${config.systemd.package}/bin/systemctl stop tuxedo-control.service";
        ExecStop = "${config.systemd.package}/bin/systemctl start tuxedo-control.service";
      };
    };
  };
}
