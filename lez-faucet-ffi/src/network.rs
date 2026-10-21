//! The LEZ networks this build can target, and everything that differs
//! between them.
//!
//! A network is not just a URL. It is a sequencer, an explorer, the upstream
//! LEZ revision whose builtin programs that sequencer runs, the ImageIDs those
//! programs hash to, and the way a faucet claim is made there. Those facts
//! move together when a testnet is reset or upgraded, so they live together
//! here rather than as loose constants scattered across the client.
//!
//! # Adding a network
//!
//! There is exactly one enabled network today: the LEZ legacy testnet, which
//! still runs the v0.2.x program set with the built-in Piñata faucet.
//!
//! The main testnet (`testnet.lez.logos.co`) moved to LEZ v0.3.x, which ships
//! neither `pinata` nor `authenticated_transfer`. It cannot be served by
//! adding a row to [`ENABLED`]:
//!
//! - its claim mechanism is not [`ClaimMechanism::Pinata`], so it needs a new
//!   [`ClaimMechanism`] variant and a client path that implements it;
//! - this crate links one LEZ revision, and the expected ImageIDs are the ones
//!   compiled from it (`programs_match_the_compiled_build` below enforces
//!   that), so a network on a different LEZ revision needs either its own
//!   build or a client that does not depend on compiled program IDs.
//!
//! When that work lands, add a [`Network`] constant for it, add it to
//! [`ENABLED`], and teach `FaucetClient` to dispatch on [`Network::claim`].
//! The UI's network table in `faucet-ui/src/FaucetBackend.cpp` mirrors this
//! one and must gain the same row.

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
}

/// The builtin program ImageIDs a network is expected to run, as 32-byte
/// little-endian hex — the same form `docs/testnet.md` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedPrograms {
    pub authenticated_transfer: &'static str,
    pub pinata: &'static str,
}

/// One LEZ network this build knows how to claim from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    /// Stable machine identifier, reported to the UI as `network`.
    pub id: &'static str,
    /// What a person reads in the UI.
    pub display_name: &'static str,
    /// Sequencer JSON-RPC endpoint. Its host is the only non-loopback host
    /// a client for this network will talk to.
    pub sequencer_url: &'static str,
    pub sequencer_host: &'static str,
    /// Block explorer root. Transactions are at `<explorer>/transaction/<hash>`.
    pub explorer_url: &'static str,
    /// Upstream `logos-execution-zone` tag the sequencer runs.
    pub lez_version: &'static str,
    /// The commit that tag points at, which is what Cargo.toml pins.
    pub lez_commit: &'static str,
    pub programs: ExpectedPrograms,
    pub claim: ClaimMechanism,
}

impl Network {
    /// The prize one claim credits, for networks whose mechanism has one.
    #[must_use]
    pub const fn prize(&self) -> u128 {
        match self.claim {
            ClaimMechanism::Pinata { prize } => prize,
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
    programs: ExpectedPrograms {
        authenticated_transfer: "fe96c4228babbe8bc578e3e25b884cacb07f8c86541f27ed676789875eef875a",
        pinata: "fc52f17a60f8b5e8de28e1a8c3133c012485011a36aef985ce24d69ff4f3528c",
    },
    claim: ClaimMechanism::Pinata { prize: 150 },
};

/// Every network this build will connect to. Order is display order.
pub const ENABLED: &[Network] = &[LEZ_LEGACY_TESTNET];

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
        // The runtime fingerprint gate compares the sequencer against the
        // *compiled* ImageIDs. A network row that disagrees with them would be
        // a network this build cannot actually claim from, so refuse it here,
        // offline, rather than on the first live call.
        for network in ENABLED {
            match network.claim {
                ClaimMechanism::Pinata { .. } => {
                    assert_eq!(
                        network.programs.authenticated_transfer,
                        program_id_hex(programs::authenticated_transfer().id()),
                        "{}: authenticated_transfer ImageID",
                        network.id
                    );
                    assert_eq!(
                        network.programs.pinata,
                        program_id_hex(programs::pinata().id()),
                        "{}: pinata ImageID",
                        network.id
                    );
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
            assert_eq!(for_host(network.sequencer_host), Some(network));
        }
        assert!(ENABLED.contains(&DEFAULT));
    }

    #[test]
    fn every_enabled_network_pays_the_compiled_prize() {
        for network in ENABLED {
            assert_eq!(network.prize(), crate::client::PRIZE, "{}", network.id);
        }
    }

    #[test]
    fn the_v0_3_testnet_is_not_a_target() {
        // LEZ v0.3.x ships no pinata program; this build cannot claim there.
        assert_eq!(for_host("testnet.lez.logos.co"), None);
    }

    #[test]
    fn explorer_links_point_at_the_transaction_page() {
        assert_eq!(
            LEZ_LEGACY_TESTNET.explorer_transaction_url("ab"),
            "https://explorer.testnet-legacy.lez.logos.co/transaction/ab"
        );
    }
}
