{
  description = "LEZ Faucet Basecamp QML view";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";
    nix-bundle-lgx.url = "github:logos-co/nix-bundle-lgx";

    # The input name must match metadata.json's dependency name.
    lez_faucet.url = "path:../faucet-module";
  };

  outputs = inputs@{ logos-module-builder, ... }:
    let
      base = logos-module-builder.lib.mkLogosQmlModule {
        src = ./.;
        configFile = ./metadata.json;
        flakeInputs = inputs;
      };
    in
    base // (
      if base ? apps then {
        apps = builtins.mapAttrs (_system: apps:
          apps // { app = apps.default; }
        ) base.apps;
      } else {}
    );
}
