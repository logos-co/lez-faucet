//! The LEZ networks this build can target, and everything that differs
//! between them.
//!
//! A network is not just a URL. It is a sequencer, an explorer, the upstream
//! LEZ revision that sequencer runs, the fingerprint its builtin programs
//! hash to, the unit its balances are denominated in, and the way a faucet
//! claim is made there. Those facts move together when a testnet is reset or
//! upgraded, so they live together here rather than as loose constants
//! scattered across the client.
//!
//! # The two enabled networks
//!
//! - The **LEZ legacy testnet** runs the v0.2.x program set with the built-in
//!   Piñata faucet. A claim there is an unsigned proof-of-work transaction the
//!   client builds and submits itself ([`ClaimMechanism::Pinata`]).
//! - The **LEZ v0.3 testnet** (`testnet.lez.logos.co`) ships neither `pinata`
//!   nor `authenticated_transfer`, and every public transaction there needs a
//!   funded, signing fee payer. The app stays keyless, so a claim there is a
//!   request to a hosted claim service that pays from a small hot account
//!   ([`ClaimMechanism::ClaimApi`]). The app still proves the outcome on chain
//!   itself: it reads the balance before, polls `getTransaction` for the hash
//!   the service reports, and requires the balance to land at exactly
//!   `before + prize`.
//!
//! This crate links one LEZ revision (v0.2.4) and uses its typed RPC client
//! for the legacy network. v0.3 changed the wire shape of `getAccount`, so the
//! v0.3 path talks raw JSON-RPC (`crate::v03`) and never depends on compiled
//! program IDs — its fingerprint is the deployed circuit's ImageID, recorded
//! here as data.
//!
//! # Adding a network
//!
//! Add a [`Network`] constant, add it to [`ENABLED`], and the host pin, the
//! fingerprint gate and the UI's network line follow. The UI's network table
//! in `faucet-ui/src/FaucetBackend.cpp` mirrors this one and must gain the
//! same row.

use serde::Serialize;

/// How a faucet claim is made on a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimMechanism {
    /// An unsigned `PublicNoSign` claim against the built-in Piñata program:
    /// solve the current proof-of-work challenge, and the program pays a fixed
    /// prize to an authenticated-transfer account. Needs no key material.
    Pinata {
        /// The exact amount one successful claim credits.
        prize: u128,
    },
    /// A request to a hosted claim service (`claimd`), which signs one native
    /// transfer from its own hot account and pays the fee. The app holds no
    /// key and proves the credit on chain itself.
    ClaimApi {
        /// Root of the service's HTTP API, without a trailing slash.
        base_url: &'static str,
        /// The exact amount one successful claim credits, in the network's
        /// smallest unit.
        prize: u128,
    },
}

/// What `getProgramIds` must report for a network to be the one this row
/// describes. Values are ImageIDs as 32-byte little-endian hex — the same
/// form `docs/testnet.md` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkFingerprint {
    /// The v0.2.x builtin set the Piñata claim depends on. These are compared
    /// against the ELFs compiled into this build, not only against this row.
    ImageIds {
        authenticated_transfer: &'static str,
        pinata: &'static str,
    },
    /// The v0.3 privacy-preserving circuit, the only program ID that network
    /// reports. Compared against this row alone; this build compiles no v0.3
    /// program.
    PpcImageId(&'static str),
}

/// One LEZ network this build knows how to claim from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    /// Stable machine identifier, reported to the UI as `network`.
    pub id: &'static str,
    /// What a person reads in the UI.
    pub display_name: &'static str,
    /// Sequencer JSON-RPC endpoint. Its host, and the claim service's host
    /// for a [`ClaimMechanism::ClaimApi`] row, are the only non-loopback hosts
    /// a client for this network will talk to.
    pub sequencer_url: &'static str,
    pub sequencer_host: &'static str,
    /// Block explorer root. Transactions are at `<explorer>/transaction/<hash>`.
    pub explorer_url: &'static str,
    /// Upstream `logos-execution-zone` tag the sequencer runs.
    pub lez_version: &'static str,
    /// The commit that tag points at.
    pub lez_commit: &'static str,
    /// Decimal places between the smallest on-chain unit and one LEZ, for
    /// display only. Every amount crosses the API as an exact integer of the
    /// smallest unit.
    pub decimals: u8,
    pub fingerprint: NetworkFingerprint,
    pub claim: ClaimMechanism,
}

impl Network {
    /// The prize one claim credits, in the smallest unit.
    #[must_use]
    pub const fn prize(&self) -> u128 {
        match self.claim {
            ClaimMechanism::Pinata { prize } | ClaimMechanism::ClaimApi { prize, .. } => prize,
        }
    }

    /// The claim mechanism's wire name, as `FaucetInfo` reports it.
    #[must_use]
    pub const fn claim_mechanism(&self) -> &'static str {
        match self.claim {
            ClaimMechanism::Pinata { .. } => "pinata",
            ClaimMechanism::ClaimApi { .. } => "claim_api",
        }
    }

    /// Root of the claim service, for a [`ClaimMechanism::ClaimApi`] row.
    #[must_use]
    pub const fn claim_api_url(&self) -> Option<&'static str> {
        match self.claim {
            ClaimMechanism::Pinata { .. } => None,
            ClaimMechanism::ClaimApi { base_url, .. } => Some(base_url),
        }
    }

    /// Explorer page for one transaction.
    #[must_use]
    pub fn explorer_transaction_url(&self, tx_hash: &str) -> String {
        format!(
            "{}/transaction/{tx_hash}",
            self.explorer_url.trim_end_matches('/')
        )
    }
}

/// The LEZ legacy testnet: LEZ `v0.2.4-lez-legacy`, with Piñata.
pub const LEZ_LEGACY_TESTNET: Network = Network {
    id: "lez-legacy-testnet",
    display_name: "LEZ legacy testnet",
    sequencer_url: "https://testnet-legacy.lez.logos.co",
    sequencer_host: "testnet-legacy.lez.logos.co",
    explorer_url: "https://explorer.testnet-legacy.lez.logos.co",
    lez_version: "v0.2.4-lez-legacy",
    lez_commit: "399793b4f99b836e40694cea34fa02790912e18e",
    decimals: 0,
    fingerprint: NetworkFingerprint::ImageIds {
        authenticated_transfer: "fe96c4228babbe8bc578e3e25b884cacb07f8c86541f27ed676789875eef875a",
        pinata: "fc52f17a60f8b5e8de28e1a8c3133c012485011a36aef985ce24d69ff4f3528c",
    },
    claim: ClaimMechanism::Pinata { prize: 150 },
};

/// The LEZ v0.3 testnet: LEZ `v0.3.1`, claims through the hosted claim
/// service. 1 LEZ = 10^9 lepta; the prize is 0.1 LEZ.
pub const LEZ_V03_TESTNET: Network = Network {
    id: "lez-v03-testnet",
    display_name: "LEZ v0.3 testnet",
    sequencer_url: "https://testnet.lez.logos.co",
    sequencer_host: "testnet.lez.logos.co",
    explorer_url: "https://explorer.testnet.lez.logos.co",
    lez_version: "v0.3.1",
    lez_commit: "1a003bfbfb89fcf987f62975cd08a401169e1b8c",
    decimals: 9,
    fingerprint: NetworkFingerprint::PpcImageId(
        "189513eb0a037edb638b3da824952e69894a4d11ab05c5956a5986181ab02721",
    ),
    claim: ClaimMechanism::ClaimApi {
        base_url: "https://rog-devbox.tail957e2.ts.net:8443",
        prize: 100_000_000,
    },
};

/// Every network this build will connect to. Order is display order.
pub const ENABLED: &[Network] = &[LEZ_LEGACY_TESTNET, LEZ_V03_TESTNET];

/// The network a client targets when nothing else is said.
pub const DEFAULT: Network = LEZ_LEGACY_TESTNET;

/// The enabled network served by `host`, if any.
#[must_use]
pub fn for_host(host: &str) -> Option<&'static Network> {
    ENABLED
        .iter()
        .find(|network| network.sequencer_host.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::program_id_hex;

    #[test]
    fn programs_match_the_compiled_build() {
        // The runtime fingerprint gate for a Piñata network compares the
        // sequencer against the *compiled* ImageIDs. A row that disagrees with
        // them would be a network this build cannot actually claim from, so
        // refuse it here, offline, rather than on the first live call. A
        // claim-service network compiles no program; its fingerprint is data
        // and is checked live against the sequencer instead.
        for network in ENABLED {
            match (network.claim, network.fingerprint) {
                (
                    ClaimMechanism::Pinata { .. },
                    NetworkFingerprint::ImageIds {
                        authenticated_transfer,
                        pinata,
                    },
                ) => {
                    assert_eq!(
                        authenticated_transfer,
                        program_id_hex(programs::authenticated_transfer().id()),
                        "{}: authenticated_transfer ImageID",
                        network.id
                    );
                    assert_eq!(
                        pinata,
                        program_id_hex(programs::pinata().id()),
                        "{}: pinata ImageID",
                        network.id
                    );
                }
                (ClaimMechanism::ClaimApi { .. }, NetworkFingerprint::PpcImageId(ppc)) => {
                    assert_eq!(
                        ppc.len(),
                        64,
                        "{}: PPC ImageID is 32 bytes of hex",
                        network.id
                    );
                    assert!(ppc.bytes().all(|byte| byte.is_ascii_hexdigit()));
                }
                (claim, fingerprint) => {
                    panic!(
                        "{}: {claim:?} cannot be gated by {fingerprint:?}",
                        network.id
                    )
                }
            }
        }
    }

    #[test]
    fn every_enabled_network_is_self_consistent() {
        let mut ids = std::collections::HashSet::new();
        for network in ENABLED {
            assert!(
                ids.insert(network.id),
                "duplicate network id {}",
                network.id
            );
            let parsed = url::Url::parse(network.sequencer_url).unwrap();
            assert_eq!(parsed.scheme(), "https", "{}", network.id);
            assert_eq!(parsed.host_str(), Some(network.sequencer_host));
            assert!(network.explorer_url.starts_with("https://"));
            assert_eq!(network.lez_commit.len(), 40);
            assert!(network.prize() > 0);
            assert_eq!(for_host(network.sequencer_host), Some(network));
            if let Some(base_url) = network.claim_api_url() {
                let parsed = url::Url::parse(base_url).unwrap();
                assert_eq!(parsed.scheme(), "https", "{}: claim API", network.id);
                assert!(!base_url.ends_with('/'), "{}: claim API root", network.id);
            }
        }
        assert!(ENABLED.contains(&DEFAULT));
    }

    #[test]
    fn each_network_pays_its_own_prize() {
        assert_eq!(LEZ_LEGACY_TESTNET.prize(), 150);
        assert_eq!(LEZ_LEGACY_TESTNET.decimals, 0);
        assert_eq!(LEZ_LEGACY_TESTNET.claim_mechanism(), "pinata");
        assert_eq!(LEZ_LEGACY_TESTNET.claim_api_url(), None);

        // 0.1 LEZ at nine decimals.
        assert_eq!(LEZ_V03_TESTNET.prize(), 100_000_000);
        assert_eq!(LEZ_V03_TESTNET.decimals, 9);
        assert_eq!(LEZ_V03_TESTNET.claim_mechanism(), "claim_api");
        assert_eq!(
            LEZ_V03_TESTNET.claim_api_url(),
            Some("https://rog-devbox.tail957e2.ts.net:8443")
        );
    }

    #[test]
    fn the_default_network_is_still_the_legacy_testnet() {
        assert_eq!(DEFAULT.id, "lez-legacy-testnet");
        assert_eq!(ENABLED[0], DEFAULT);
    }

    #[test]
    fn both_testnet_hosts_resolve_to_their_rows() {
        assert_eq!(
            for_host("testnet-legacy.lez.logos.co").map(|n| n.id),
            Some("lez-legacy-testnet")
        );
        assert_eq!(
            for_host("testnet.lez.logos.co").map(|n| n.id),
            Some("lez-v03-testnet")
        );
        assert_eq!(for_host("evil.example.com"), None);
    }

    #[test]
    fn explorer_links_point_at_the_transaction_page() {
        assert_eq!(
            LEZ_LEGACY_TESTNET.explorer_transaction_url("ab"),
            "https://explorer.testnet-legacy.lez.logos.co/transaction/ab"
        );
        assert_eq!(
            LEZ_V03_TESTNET.explorer_transaction_url("ab"),
            "https://explorer.testnet.lez.logos.co/transaction/ab"
        );
    }
}
