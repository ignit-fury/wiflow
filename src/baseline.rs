//! Phase 1 benchmark baseline: representative fixtures + deterministic-path
//! timing/allocation measurement + current LLM-routing accounting.
//!
//! No behavior change: [`deterministic_clean`] replicates the exact
//! `Stt::transcribe` tail (`stt.rs`), and [`would_call_llm`] /
//! [`would_synthesize`] mirror the current call guards. Phase 4 replaces the
//! routing mirrors with the real analyzer; Phase 2 may change the internals
//! as long as recorded outputs stay identical.

use std::hint::black_box;
use std::time::Instant;

/// One representative transcript.
pub struct Fixture {
    pub name: &'static str,
    pub input: &'static str,
}

/// The 12 Phase 1 categories (real-world dictation shapes).
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        name: "already_clean",
        input: "hello how are you",
    },
    Fixture {
        name: "capitalization",
        input: "hello world. this is wiflow.",
    },
    Fixture {
        name: "standalone_i",
        input: "i think i can do it",
    },
    Fixture {
        name: "hallucination_token",
        input: "[BLANK_AUDIO]",
    },
    Fixture {
        name: "token_embedded",
        input: "hello [MUSIC] world",
    },
    Fixture {
        name: "repetition",
        input: "the the deployment is ready",
    },
    Fixture {
        name: "filler",
        input: "um so I think we should deploy",
    },
    Fixture {
        name: "self_correction",
        input: "the deploy is Thursday no actually Wednesday",
    },
    Fixture {
        name: "spoken_math",
        input: "A plus B",
    },
    Fixture {
        name: "long_natural",
        input: "Good morning everyone. Thanks for joining the status meeting a few minutes early. Yesterday we finished the migration, and all the checks are green, so today I want to focus on the rollout plan, the remaining risks, and who owns each follow-up before Friday.",
    },
    Fixture {
        name: "proper_nouns",
        input: "Playboy Carti and Rather Lie",
    },
    Fixture {
        name: "punct_numbers",
        input: "version 1.0 is ready. send it today.",
    },
];

/// Exact replica of the `Stt::transcribe` deterministic tail: token strip →
/// post_process → spoken math. Deliberately duplicated call order (not shared
/// code) so Phase 2 refactors are verified against this behavior.
pub fn deterministic_clean(input: &str) -> String {
    if input.is_empty() {
        return String::new();
    }
    let stripped = crate::stt::strip_hallucination_tokens(input);
    let processed = crate::stt::post_process(&stripped);
    crate::stt::plus_to_symbol(&processed)
}

/// Current effective routing rule: an LLM call happens iff cleanup is enabled
/// AND the deterministic output is non-empty (`clean_chain` early-returns on
/// empty input). Takes the DETERMINISTIC output, not the raw transcript.
pub fn would_call_llm(deterministic_out: &str, cleanup_enabled: bool) -> bool {
    cleanup_enabled && !deterministic_out.trim().is_empty()
}

/// Current effective context rule: one extra model call iff cleanup+context
/// are enabled, the app is known, and a cloud key exists (local path
/// removed — all-local setups never synthesize).
pub fn would_synthesize(
    cleanup_enabled: bool,
    context_enabled: bool,
    app_vague: bool,
    cloud_key_present: bool,
) -> bool {
    cleanup_enabled && context_enabled && !app_vague && cloud_key_present
}

// --- Allocation accounting (test-only) -------------------------------------

#[cfg(test)]
use std::alloc::{GlobalAlloc, Layout, System};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

/// Test-only counting allocator: reports allocations inside the measured
/// section. Takes the minimum of several rounds (test threads only add
/// noise, never remove it). Reset/read via the helpers below.
#[cfg(test)]
struct CountingAlloc;

#[cfg(test)]
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[cfg(test)]
#[global_allocator]
static COUNTING: CountingAlloc = CountingAlloc {};

#[cfg(test)]
fn alloc_snapshot() -> (u64, u64) {
    (
        ALLOC_COUNT.load(Ordering::Relaxed),
        ALLOC_BYTES.load(Ordering::Relaxed),
    )
}

#[cfg(test)]
fn min_allocs(f: impl Fn() -> String) -> (u64, u64) {
    let mut best = (u64::MAX, u64::MAX);
    for _ in 0..5 {
        let (c0, b0) = alloc_snapshot();
        black_box(f());
        let (c1, b1) = alloc_snapshot();
        let d = (c1 - c0, b1 - b0);
        if d < best {
            best = d;
        }
    }
    best
}

#[cfg(test)]
fn mean_ns_per_iter(input: &str, iters: u64) -> u64 {
    // Warmup (allocator + branch predictors), then timed loop.
    for _ in 0..100 {
        black_box(deterministic_clean(black_box(input)));
    }
    let t0 = Instant::now();
    for _ in 0..iters {
        black_box(deterministic_clean(black_box(input)));
    }
    t0.elapsed().as_nanos() as u64 / iters
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pinned Phase 1 outputs: Phase 2+ refactors must reproduce these
    /// byte-identically (any diff = behavior change needing justification).
    #[test]
    fn baseline_outputs_pinned() {
        let cases = [
            ("hello how are you", "Hello how are you"),
            (
                "hello world. this is wiflow.",
                "Hello world. This is wiflow.",
            ),
            ("i think i can do it", "I think I can do it"),
            ("[BLANK_AUDIO]", ""),
            ("hello [MUSIC] world", "Hello world"),
            ("the the deployment is ready", "The the deployment is ready"),
            (
                "um so I think we should deploy",
                "Um so I think we should deploy",
            ),
            (
                "the deploy is Thursday no actually Wednesday",
                "The deploy is Thursday no actually Wednesday",
            ),
            ("A plus B", "A+B"),
            (
                "Playboy Carti and Rather Lie",
                "Playboy Carti and Rather Lie",
            ),
            (
                "version 1.0 is ready. send it today.",
                "Version 1.0 is ready. Send it today.",
            ),
        ];
        for (input, want) in cases {
            assert_eq!(deterministic_clean(input), want, "fixture {input:?}");
        }
        // Long fixture: pinned separately (readability).
        let long_in = FIXTURES[9].input;
        assert!(deterministic_clean(long_in).starts_with("Good morning everyone."));
        assert!(deterministic_clean(long_in).ends_with("before Friday."));
    }

    #[test]
    fn baseline_routing_mirrors_current_behavior() {
        // Today: everything non-empty goes to LLM; context whenever enabled
        // with a cloud key. Phase 4 must flip simple cases to false.
        assert!(would_call_llm("Hello world", true));
        assert!(!would_call_llm("", true));
        assert!(!would_call_llm("   ", true));
        assert!(!would_call_llm("Hello", false));
        assert!(would_synthesize(true, true, false, true));
        assert!(!would_synthesize(true, true, false, false)); // all-local
        assert!(!would_synthesize(true, true, true, true)); // vague app
        assert!(!would_synthesize(true, false, false, true));
    }

    #[test]
    fn baseline_report() {
        // Baseline record: per-fixture deterministic output + latency +
        // allocations + current routing decision (default config: cleanup on,
        // cloud key present, app known).
        let mut llm_calls = 0;
        for f in FIXTURES {
            let out = deterministic_clean(f.input);
            let iters = if f.input.len() > 120 { 300 } else { 2000 };
            let ns = mean_ns_per_iter(f.input, iters);
            let (allocs, bytes) = min_allocs(|| deterministic_clean(f.input));
            let llm = would_call_llm(&out, true);
            let ctx = would_synthesize(true, true, false, true);
            llm_calls += llm as usize;
            eprintln!(
                "BASELINE {:<16} in={:>3}B ns/iter={:>6} allocs={:>2} bytes={:>5} llm={llm} ctx={ctx} out={out:?}",
                f.name,
                f.input.len(),
                ns,
                allocs,
                bytes,
            );
        }
        eprintln!(
            "BASELINE llm_calls={llm_calls}/{} ctx_calls=12/12 (all-local: 0/12)",
            FIXTURES.len()
        );
    }
}
