//! Piñata proof-of-work.
//!
//! The deployed guest accepts a `u128` solution whose validity condition is
//! that the first `difficulty` **bytes** of `SHA256(seed || solution_le)` are
//! zero. This module mirrors that check byte for byte; see the guest at
//! `lez/programs/pinata/src/main.rs` in the pinned LEZ revision.

use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use sha2::{Digest as _, Sha256};

use crate::error::{ApiError, ApiResult, ErrorCode};

/// Challenge data as stored in the Piñata account: one difficulty byte
/// followed by a 32-byte seed.
pub const CHALLENGE_LEN: usize = 33;

/// The protocol permits 0..=32. Anything above this is refused by the client
/// because the work is not bounded in a way we are willing to impose on a
/// user's machine.
pub const MAX_SUPPORTED_DIFFICULTY: u8 = 3;

/// How often the solver checks whether it should stop.
const CANCEL_CHECK_INTERVAL: u64 = 4_096;

/// A validated Piñata challenge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Challenge {
    pub difficulty: u8,
    pub seed: [u8; 32],
}

impl Challenge {
    /// Parse the 33 raw bytes of the Piñata account's data field.
    pub fn parse(data: &[u8]) -> ApiResult<Self> {
        let bytes: [u8; CHALLENGE_LEN] = data.try_into().map_err(|_| {
            ApiError::new(
                ErrorCode::InvalidChallenge,
                "The Piñata account's challenge data is not the expected 33 bytes.",
            )
        })?;
        let difficulty = bytes[0];
        if difficulty > 32 {
            return Err(ApiError::new(
                ErrorCode::InvalidChallenge,
                "The Piñata challenge declares a difficulty larger than a SHA-256 digest.",
            ));
        }
        let mut seed = [0_u8; 32];
        seed.copy_from_slice(&bytes[1..]);
        Ok(Self { difficulty, seed })
    }

    /// Reject work this client will not attempt.
    pub fn ensure_supported(&self) -> ApiResult<()> {
        if self.difficulty > MAX_SUPPORTED_DIFFICULTY {
            return Err(ApiError::new(
                ErrorCode::UnsupportedDifficulty,
                format!(
                    "The Piñata challenge difficulty is {}, above the {MAX_SUPPORTED_DIFFICULTY} this app supports.",
                    self.difficulty
                ),
            ));
        }
        Ok(())
    }

    /// A short, non-reversible identifier for a challenge.
    ///
    /// Used in `outcome_unknown` details so a support conversation can compare
    /// two attempts without the full challenge being echoed around.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update([self.difficulty]);
        hasher.update(self.seed);
        let digest: [u8; 32] = hasher.finalize().into();
        digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    #[must_use]
    pub fn is_valid_solution(&self, solution: u128) -> bool {
        valid_solution(usize::from(self.difficulty), &self.seed, solution)
    }
}

/// The guest's validity check, reproduced exactly.
///
/// Preimage is 48 bytes: the 32-byte seed as stored, followed by the solution's
/// 16 little-endian bytes.
#[must_use]
pub fn valid_solution(difficulty: usize, seed: &[u8; 32], solution: u128) -> bool {
    let mut input = [0_u8; 48];
    input[..32].copy_from_slice(seed);
    input[32..].copy_from_slice(&solution.to_le_bytes());
    let digest: [u8; 32] = Sha256::digest(input).into();
    digest[..difficulty].iter().all(|byte| *byte == 0)
}

/// Pick an unpredictable starting point for the nonce search.
///
/// Not a security primitive — it only needs to differ between processes. See
/// [`solve_bounded`] for why scanning from a fixed point is harmful.
fn random_start() -> u128 {
    use std::hash::{BuildHasher as _, Hasher as _};

    let mix = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos().into()),
        );
        hasher.finish()
    };
    (u128::from(mix(1)) << 64) | u128::from(mix(2))
}

/// Search for a solution on the calling thread, starting at `start`.
///
/// `stop` is polled every [`CANCEL_CHECK_INTERVAL`] candidates so cancellation
/// and deadlines are observed promptly without measurably slowing the hash
/// loop.
///
/// The starting point matters. A claim transaction carries no signature, no
/// nonce and no sender, so its bytes are determined entirely by the recipient
/// and the solution. Two clients that both scanned upward from zero against the
/// same challenge would find the *same* solution, build byte-identical
/// transactions, and therefore share one transaction hash — which would make
/// "was my transaction included?" unanswerable for both of them, and let each
/// report the other's outcome as its own. Starting somewhere unpredictable
/// keeps every client's transaction, and so every client's evidence, distinct.
pub fn solve_bounded(
    challenge: Challenge,
    start: u128,
    max_attempts: u64,
    stop: &dyn Fn() -> bool,
) -> ApiResult<u128> {
    challenge.ensure_supported()?;
    if max_attempts == 0 {
        return Err(ApiError::internal("solver attempt budget must be positive"));
    }

    let difficulty = usize::from(challenge.difficulty);
    for attempt in 0..max_attempts {
        if attempt % CANCEL_CHECK_INTERVAL == 0 && stop() {
            return Err(ApiError::new(
                ErrorCode::Cancelled,
                "The request was cancelled.",
            ));
        }
        let solution = start.wrapping_add(u128::from(attempt));
        if valid_solution(difficulty, &challenge.seed, solution) {
            return Ok(solution);
        }
    }

    Err(ApiError::new(
        ErrorCode::SolveAttemptLimit,
        "Could not find a proof-of-work solution within this app's attempt budget.",
    ))
}

/// Run [`solve_bounded`] on the blocking pool under a deadline.
///
/// A blocking task cannot be aborted once it is running, so on timeout we
/// signal it cooperatively and then **await its exit** before returning. That
/// guarantee is the point of this function: a timed-out solve must never leave
/// a core spinning behind the user's back.
pub async fn solve_with_deadline(
    challenge: Challenge,
    max_attempts: u64,
    deadline: Duration,
    cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
    worker_active: Option<Arc<AtomicBool>>,
) -> ApiResult<u128> {
    challenge.ensure_supported()?;

    let timed_out = Arc::new(AtomicBool::new(false));
    let worker_timed_out = Arc::clone(&timed_out);
    let worker_cancelled = Arc::clone(&cancelled);
    let activity = worker_active.clone();

    let mut task = tokio::task::spawn_blocking(move || {
        struct ActivityGuard(Option<Arc<AtomicBool>>);
        impl Drop for ActivityGuard {
            fn drop(&mut self) {
                if let Some(active) = &self.0 {
                    active.store(false, Ordering::SeqCst);
                }
            }
        }
        if let Some(active) = &activity {
            active.store(true, Ordering::SeqCst);
        }
        let _guard = ActivityGuard(activity);

        solve_bounded(challenge, random_start(), max_attempts, &|| {
            worker_timed_out.load(Ordering::Relaxed) || worker_cancelled()
        })
    });

    tokio::select! {
        result = &mut task => {
            result.map_err(|_| ApiError::internal("the proof-of-work solver task failed"))?
        }
        () = tokio::time::sleep(deadline) => {
            timed_out.store(true, Ordering::SeqCst);
            let _ = task.await;
            Err(ApiError::new(
                ErrorCode::SolveTimeout,
                "Proof-of-work took longer than this app allows for one attempt.",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(difficulty: u8, seed_byte: u8) -> Challenge {
        Challenge {
            difficulty,
            seed: [seed_byte; 32],
        }
    }

    #[test]
    fn parses_the_thirty_three_byte_layout() {
        let mut data = vec![3_u8];
        data.extend_from_slice(&[0xAB; 32]);
        let parsed = Challenge::parse(&data).unwrap();
        assert_eq!(parsed.difficulty, 3);
        assert_eq!(parsed.seed, [0xAB; 32]);
    }

    #[test]
    fn rejects_wrong_length_data() {
        assert_eq!(
            Challenge::parse(&[]).unwrap_err().code,
            ErrorCode::InvalidChallenge
        );
        assert_eq!(
            Challenge::parse(&[0; 32]).unwrap_err().code,
            ErrorCode::InvalidChallenge
        );
        assert_eq!(
            Challenge::parse(&[0; 34]).unwrap_err().code,
            ErrorCode::InvalidChallenge
        );
    }

    #[test]
    fn rejects_impossible_and_unsupported_difficulty_with_distinct_codes() {
        let mut data = vec![33_u8];
        data.extend_from_slice(&[0; 32]);
        assert_eq!(
            Challenge::parse(&data).unwrap_err().code,
            ErrorCode::InvalidChallenge,
            "a difficulty above the digest size is a malformed challenge"
        );

        assert_eq!(
            challenge(MAX_SUPPORTED_DIFFICULTY + 1, 0)
                .ensure_supported()
                .unwrap_err()
                .code,
            ErrorCode::UnsupportedDifficulty,
            "a protocol-legal but too-hard challenge is a client limit, not corruption"
        );
    }

    #[test]
    fn solves_and_the_solution_verifies_against_the_guest_rule() {
        let work = challenge(1, 0x42);
        let solution = solve_bounded(work, 0, 1 << 20, &|| false).unwrap();
        assert!(work.is_valid_solution(solution));
        assert!(
            (0..solution).all(|candidate| !work.is_valid_solution(candidate)),
            "the solver must return the first valid solution"
        );
    }

    #[test]
    fn reproduces_the_live_mainnet_challenge_solution() {
        // Seed read from the live Piñata account on 2026-07-27 at difficulty 3.
        // Pinning it here means a change to the preimage layout, the byte order,
        // or the digest fails loudly rather than silently mining garbage.
        let seed_hex = "becf635618bfedd94e94bcb37a9a9e532aac84215ffc95de29f2dd36cfcb033f";
        let mut seed = [0_u8; 32];
        for (index, byte) in seed.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&seed_hex[index * 2..index * 2 + 2], 16).unwrap();
        }
        let live = Challenge {
            difficulty: 3,
            seed,
        };
        assert!(live.is_valid_solution(25_385_721));
        assert!(!live.is_valid_solution(25_385_720));
    }

    #[tokio::test]
    async fn two_solves_of_one_challenge_do_not_collide() {
        // A claim transaction has no signature, no nonce and no sender, so the
        // solution is the only thing that distinguishes two clients funding the
        // same recipient. If both scanned from zero they would produce the same
        // transaction and the same hash, and neither could tell whose claim had
        // been included. Distinct solutions keep the evidence separable.
        let work = challenge(2, 0x11);
        let solve = || {
            solve_with_deadline(
                work,
                1 << 24,
                Duration::from_secs(30),
                Arc::new(|| false),
                None,
            )
        };
        let (first, second) = (solve().await.unwrap(), solve().await.unwrap());
        assert!(work.is_valid_solution(first));
        assert!(work.is_valid_solution(second));
        assert_ne!(
            first, second,
            "independent solves must not return the same nonce"
        );
    }

    #[test]
    fn a_randomized_start_still_finds_a_valid_solution() {
        let work = challenge(2, 0x11);
        for start in [0, 1, u128::MAX - 10, 1 << 100] {
            let solution = solve_bounded(work, start, 1 << 24, &|| false).unwrap();
            assert!(work.is_valid_solution(solution));
        }
    }

    #[test]
    fn random_start_varies_between_calls() {
        let samples: std::collections::HashSet<u128> = (0..64).map(|_| random_start()).collect();
        assert!(
            samples.len() > 32,
            "the nonce start must vary; got {} distinct values from 64 draws",
            samples.len()
        );
    }

    #[test]
    fn zero_difficulty_is_satisfied_immediately() {
        assert_eq!(solve_bounded(challenge(0, 0), 0, 1, &|| false).unwrap(), 0);
    }

    #[test]
    fn respects_the_attempt_budget() {
        let work = challenge(2, 0x42);
        assert_eq!(
            solve_bounded(work, 0, 1, &|| false).unwrap_err().code,
            ErrorCode::SolveAttemptLimit
        );
    }

    #[test]
    fn stops_when_asked_to() {
        let work = challenge(3, 0x42);
        assert_eq!(
            solve_bounded(work, 0, u64::MAX, &|| true).unwrap_err().code,
            ErrorCode::Cancelled
        );
    }

    #[test]
    fn fingerprint_is_stable_and_hides_the_seed() {
        let work = challenge(3, 0x42);
        let fingerprint = work.fingerprint();
        assert_eq!(fingerprint, work.fingerprint());
        assert_eq!(fingerprint.len(), 16);
        assert_ne!(fingerprint, challenge(3, 0x43).fingerprint());
        assert!(!fingerprint.contains("4242"));
    }

    #[tokio::test]
    async fn a_cancelled_solve_has_stopped_its_worker_before_returning() {
        let worker_active = Arc::new(AtomicBool::new(false));
        let error = solve_with_deadline(
            challenge(MAX_SUPPORTED_DIFFICULTY, 0x42),
            u64::MAX,
            Duration::from_secs(30),
            Arc::new(|| true),
            Some(Arc::clone(&worker_active)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Cancelled);
        assert!(
            !worker_active.load(Ordering::SeqCst),
            "no orphaned solver may remain"
        );
    }

    #[tokio::test]
    async fn a_timed_out_solve_has_stopped_its_worker_before_returning() {
        let worker_active = Arc::new(AtomicBool::new(false));
        let error = solve_with_deadline(
            challenge(MAX_SUPPORTED_DIFFICULTY, 0x42),
            u64::MAX,
            Duration::ZERO,
            Arc::new(|| false),
            Some(Arc::clone(&worker_active)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SolveTimeout);
        assert!(
            !worker_active.load(Ordering::SeqCst),
            "no orphaned solver may remain"
        );
    }

    #[tokio::test]
    async fn unsupported_difficulty_never_reaches_the_blocking_pool() {
        let worker_active = Arc::new(AtomicBool::new(false));
        let error = solve_with_deadline(
            challenge(MAX_SUPPORTED_DIFFICULTY + 1, 0),
            u64::MAX,
            Duration::from_secs(30),
            Arc::new(|| false),
            Some(Arc::clone(&worker_active)),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedDifficulty);
        assert!(!worker_active.load(Ordering::SeqCst));
    }
}
