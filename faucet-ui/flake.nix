{
  description = "LEZ Faucet Basecamp QML view";

  inputs = {
    logos-module-builder.url = "github:logos-co/logos-module-builder";

    # This is the packaging tool, and its pin is load-bearing in a way that is
    # not obvious: bundle.sh copies metadata.json's descriptive fields into the
    # bundled manifest.json through a hand-written key allowlist, so a pin that
    # predates a key drops that key without failing anything. That is how the
    # sibling repo shipped swap v0.3.0 with no `display_name`
    # (logos-co/eth-lez-atomic-swaps#60). Keep this pin current, and note that
    # `nix flake update` alone would not have saved that release — CI's
    # manifest round trip (scripts/check-lgx-manifest.py) is what actually
    # catches it.
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
