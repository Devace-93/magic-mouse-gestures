{
  description = "Apple Magic Mouse multitouch gestures for Linux/Wayland";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
    in
    {
      packages = forAllSystems (pkgs: rec {
        magic-mouse-gestures = pkgs.rustPlatform.buildRustPackage {
          pname = "magic-mouse-gestures";
          inherit version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;

          # The engine tests create uinput devices, which the sandbox lacks.
          checkFlags = [ "--skip" "engine::" ];

          nativeBuildInputs = [ pkgs.wrapGAppsHook4 pkgs.gobject-introspection ];
          buildInputs = [ pkgs.gtk4 pkgs.libadwaita ];
          dontWrapGApps = true;

          postInstall =
            let python = pkgs.python3.withPackages (ps: [ ps.pygobject3 ]);
            in ''
              install -Dm755 ui/magic-mouse-gestures-settings $out/bin/magic-mouse-gestures-settings
              substituteInPlace $out/bin/magic-mouse-gestures-settings \
                --replace-fail "#!/usr/bin/python3" "#!${python}/bin/python3"
              wrapProgram $out/bin/magic-mouse-gestures-settings "''${gappsWrapperArgs[@]}"
              install -Dm644 packaging/org.magicmouse.Gestures.desktop $out/share/applications/org.magicmouse.Gestures.desktop
              install -Dm644 packaging/60-magic-mouse-gestures.rules $out/lib/udev/rules.d/60-magic-mouse-gestures.rules
              install -Dm644 packaging/magic-mouse-gestures.conf $out/lib/modules-load.d/magic-mouse-gestures.conf
            '';

          meta = with pkgs.lib; {
            description = "Apple Magic Mouse multitouch gestures (workspace swipes, pinch, taps) for Linux/Wayland";
            homepage = "https://github.com/Devace-93/magic-mouse-gestures";
            license = licenses.mit;
            platforms = platforms.linux;
            mainProgram = "magic-mouse-gestures";
          };
        };
        default = magic-mouse-gestures;
      });

      # NixOS: add the flake as an input and
      #   imports = [ magic-mouse-gestures.nixosModules.default ];
      #   services.magic-mouse-gestures.enable = true;
      nixosModules.default = { config, lib, pkgs, ... }:
        let cfg = config.services.magic-mouse-gestures;
        in {
          options.services.magic-mouse-gestures = {
            enable = lib.mkEnableOption "the Magic Mouse gestures daemon";
            package = lib.mkOption {
              type = lib.types.package;
              default = self.packages.${pkgs.system}.magic-mouse-gestures;
              description = "Package providing magic-mouse-gestures.";
            };
          };
          config = lib.mkIf cfg.enable {
            boot.kernelModules = [ "uinput" ];
            services.udev.packages = [ cfg.package ];
            environment.systemPackages = [ cfg.package ];
            systemd.user.services.magic-mouse-gestures = {
              description = "Magic Mouse gestures daemon (userspace driver)";
              wantedBy = [ "default.target" ];
              serviceConfig = {
                ExecStart = "${cfg.package}/bin/magic-mouse-gestures run";
                Restart = "on-failure";
                RestartSec = 2;
                Nice = -5;
              };
            };
          };
        };
    };
}
