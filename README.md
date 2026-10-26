# LEZ Faucet

A Basecamp app that funds one public account on a Logos Execution Zone
testnet. Pick a network, paste a public LEZ address, press the button.

## Install

Add this repository URL in Basecamp's App Manager:

```text
https://raw.githubusercontent.com/logos-co/lez-faucet/main/logos-repo.json
```

Install `lez_faucet` (core) first, then `lez_faucet_ui`, and restart Basecamp.
Both are released at the same version; the current one is in each module's
`metadata.json`. Details and troubleshooting:
[docs/community-install.md](docs/community-install.md).

## Networks

| | LEZ legacy testnet | LEZ v0.3 testnet |
| --- | --- | --- |
| Sequencer | `https://testnet-legacy.lez.logos.co` | `https://testnet.lez.logos.co` |
| Explorer | `https://explorer.testnet-legacy.lez.logos.co` | `https://explorer.testnet.lez.logos.co` |
| LEZ | `v0.2.4-lez-legacy` | `v0.3.1` |
| Prize | 150 LEZ | 0.1 LEZ (100,000,000 lepta) |
| How | unsigned proof-of-work claim against the on-chain Piñata | request to the hosted claim service `claimd` |

The two testnets are separate chains. An address is valid on both, so check
which sequencer your wallet is configured for.

- **Legacy:** the app builds and submits the Piñata claim itself. The recipient
  must already be an initialized public account; the app shows the owner the
  `wallet auth-transfer init` command when it is not.
- **v0.3:** there is no Piñata and every transaction needs a funded fee payer,
  so the app asks `claimd` (`https://rog-devbox.tail957e2.ts.net:8443`, contract
  in [claimd/README.md](claimd/README.md)), which sends one native transfer from
  a small hot account. Limits: one claim per account per day, a few per hour
  overall. No initialization needed.

On either network the app holds no key, stores nothing, and shows a receipt
only after it has seen the transaction on chain and the balance up by exactly
the prize. The full network contract, including how to move the pins when a
testnet upgrades, is in [docs/testnet.md](docs/testnet.md).

## Layout

```text
lez-faucet-ffi/  Rust faucet client and C ABI
faucet-module/   C++ Basecamp core module (`lez_faucet`)
faucet-ui/       QML view module (`lez_faucet_ui`)
claimd/          the v0.3 claim service (own workspace, links LEZ v0.3.1)
```

## Development

Needs Nix with flakes, Rust 1.98+, `libclang`, and logos-scaffold CLI 0.1.1.

```sh
./scripts/scaffold-setup.sh
cargo fmt --all -- --check
cargo test -p lez-faucet-ffi
lgs basecamp build --variant lgx --module lez_faucet
lgs basecamp build --variant lgx --module lez_faucet_ui
```

Read-only tests against the live testnets:

```sh
cargo test -p lez-faucet-ffi --test live_public_testnet -- --ignored --nocapture
cargo test -p lez-faucet-ffi --test live_v03_testnet -- --ignored --nocapture
```

The single write test spends from the shared pool and runs only when
`LEZ_FAUCET_LIVE_RECIPIENT=Public/<id>` is set; run it with `--release`.

Check a balance without a wallet:

```sh
./scripts/lez-balance.sh Public/<account-id>
```

## Release

Bump both `metadata.json` files in a PR and merge. Once CI on `main` is green,
`release-on-merge.yml` publishes core then UI for `darwin-arm64`, `linux-amd64`
and `linux-arm64`, and rebuilds the catalog index. See [docs/releasing.md](docs/releasing.md) and
[CHANGELOG.md](CHANGELOG.md).

## License

MIT or Apache-2.0, at your option. See `LICENSE-MIT`, `LICENSE-APACHE`, and
`NOTICE`.
