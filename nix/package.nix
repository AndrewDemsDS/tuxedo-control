{ lib, rustPlatform, pkg-config, wrapGAppsHook4, gtk4, libadwaita, glib
, makeDesktopItem, copyDesktopItems }:

rustPlatform.buildRustPackage {
  pname = "tuxedo-control";
  version = "0.3.1";

  src = lib.cleanSource ../.;
  cargoLock.lockFile = ../Cargo.lock;

  # GTK4/libadwaita for the GUI; wrapGAppsHook4 wraps tuxedo-gui with the right
  # GSETTINGS/GDK/typelib env so it runs as an installed binary (no nix-shell needed).
  nativeBuildInputs = [ pkg-config wrapGAppsHook4 copyDesktopItems ];
  buildInputs = [ gtk4 libadwaita glib ];

  # Builds the whole workspace: tuxedo-controld, tuxedo-prober, tuxedo-tui, tuxedo-gui.
  desktopItems = [
    (makeDesktopItem {
      name = "tuxedo-control";
      desktopName = "TUXEDO Control";
      comment = "Fan and performance control for TUXEDO laptops";
      exec = "tuxedo-gui";
      icon = "preferences-system";
      categories = [ "System" "Settings" "HardwareSettings" ];
      keywords = [ "fan" "tuxedo" "performance" "thermal" ];
    })
  ];

  meta = {
    description = "Declarative fan/performance control for TUXEDO Uniwill laptops via /dev/tuxedo_io";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "tuxedo-gui";
  };
}
