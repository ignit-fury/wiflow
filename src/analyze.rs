//! Phase 3 deterministic cleanup analyzer: pure-Rust heuristics deciding
//! whether an LLM cleanup call is worth its latency.
//!
//! Design: conservative and explainable. Every reason is a cheap word- or
//! substring-level signal the codebase already knows about (see the cleanup
//! prompt rules). A firing reason means "deterministic output kept something
//! the LLM prompt explicitly handles" — never a rewrite attempt.
//!
//! Bias: a false positive costs one LLM call (safe: LLM preserves clean
//! text); a false negative injects broken dictation. Borderline patterns
//! (bare "like", single acronyms, non-ASCII words) deliberately do NOT fire.

/// Firing heuristic flags. Each maps to a rule the LLM cleanup prompt owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CleanupReasons {
    /// um/uh/eh/erm standalone, or "you know" (prompt rule 3).
    pub filler: bool,
    /// Adjacent duplicate words: "the the" (prompt rule 3).
    pub repetition: bool,
    /// "no actually", "i mean", "sorry", "scratch that", … (prompt rule 4).
    pub self_correction: bool,
    /// Trailing filler / dangling correction ("…so uh", "…I mean").
    pub abandoned: bool,
    /// Spoken numbers the LLM must digitize (prompt rule 8).
    pub number_words: bool,
    /// Spoken punctuation / list requests (prompt rules 6, 9).
    pub spoken_punct: bool,
    /// Spoken code tokens (prompt rule 7).
    pub code_words: bool,
    /// '@', URLs — formatting the LLM must get right (prompt rule 10).
    pub email_url: bool,
    /// 2+ ALL-CAPS words (len ≥ 3): case restoration beyond caps-fix.
    pub all_caps: bool,
    /// Repeated punctuation runs ("!!", "??", ".."): ASR junk or emphasis.
    pub punct_runs: bool,
    /// Whisper annotation removed ("[BLANK_AUDIO]"). Informational only —
    /// never routes to LLM by itself.
    pub hallucination_removed: bool,
}

impl CleanupReasons {
    /// Weighted sum, capped at 100. Weights rank semantic-risk signals
    /// (self-correction) above cosmetic ones (caps). Informational:
    /// routing uses [`CleanupAnalysis::needs_llm`], not the score.
    pub fn score(self) -> u8 {
        let mut s = 0u16;
        if self.filler {
            s += 20;
        }
        if self.repetition {
            s += 25;
        }
        if self.self_correction {
            s += 40;
        }
        if self.abandoned {
            s += 15;
        }
        if self.number_words {
            s += 15;
        }
        if self.spoken_punct {
            s += 20;
        }
        if self.code_words {
            s += 20;
        }
        if self.email_url {
            s += 15;
        }
        if self.all_caps {
            s += 10;
        }
        if self.punct_runs {
            s += 10;
        }
        s.min(100) as u8
    }

    /// Short reason names for `cleanup route=… reason=…` logging.
    pub fn names(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.filler {
            out.push("filler");
        }
        if self.repetition {
            out.push("repetition");
        }
        if self.self_correction {
            out.push("self_correction");
        }
        if self.abandoned {
            out.push("abandoned");
        }
        if self.number_words {
            out.push("number_words");
        }
        if self.spoken_punct {
            out.push("spoken_punct");
        }
        if self.code_words {
            out.push("code_words");
        }
        if self.email_url {
            out.push("email_url");
        }
        if self.all_caps {
            out.push("all_caps");
        }
        if self.punct_runs {
            out.push("punct_runs");
        }
        if self.hallucination_removed {
            out.push("hallucination_removed");
        }
        out
    }
}

/// Deterministic cleanup result + routing decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupAnalysis {
    /// Fully cleaned text (same bytes `deterministic_clean` produces).
    pub text: String,
    /// Deterministic pass changed something vs the raw input.
    pub changed: bool,
    /// An LLM call is expected to add value.
    pub needs_llm: bool,
    /// Why (empty when routing deterministic).
    pub reasons: CleanupReasons,
}

/// Pipeline routing decision. Both arms carry the full analysis (text +
/// reasons for logging). `Direct` skips context synthesis and every provider
/// call; `Llm` runs the existing chain (context → providers → sanitizer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupRoute {
    Direct(CleanupAnalysis),
    Llm(CleanupAnalysis),
}

/// Decide the route for STT output. Cleanup disabled → always direct (the
/// old chain returned its input unchanged in that case, and the deterministic
/// stages are idempotent so re-running them changes nothing — see
/// `test_deterministic_idempotent`).
pub fn decide_route(stt_text: &str, cfg: &crate::config::Config) -> CleanupRoute {
    if !cfg.cleanup_enabled {
        return CleanupRoute::Direct(analyze_transcript(stt_text));
    }
    let a = analyze_transcript(stt_text);
    if a.needs_llm {
        CleanupRoute::Llm(a)
    } else {
        CleanupRoute::Direct(a)
    }
}

impl CleanupAnalysis {
    /// Transcript carries spelling/format-sensitive content a context hint
    /// could actually help with: addresses/URLs, code tokens, or a possible
    /// proper noun (mid-sentence capital like "Priya"). Ordinary prose
    /// returns false — describing it burns a model call for zero signal.
    pub fn wants_context(&self) -> bool {
        self.reasons.email_url || self.reasons.code_words || has_mid_sentence_capital(&self.text)
    }
}

/// Possible proper noun: non-sentence-initial word with an uppercase first
/// letter followed by lowercase ("Priya", "Thursday", "English"). Sentence
/// starts, single letters ("I"), contractions ("I'm") and acronyms ("API",
/// "PM") never fire.
fn has_mid_sentence_capital(text: &str) -> bool {
    let mut sentence_start = true;
    for w in text.split_whitespace() {
        if !sentence_start {
            let core = core_word(w);
            let mut ch = core.chars();
            if matches!(ch.next(), Some(c) if c.is_uppercase())
                && matches!(ch.next(), Some(c) if c.is_lowercase())
            {
                return true;
            }
        }
        sentence_start = false;
        if w.ends_with(['.', '?', '!']) {
            sentence_start = true;
        }
    }
    false
}

/// App allowlist for context synthesis: destinations where a hint has proven
/// value (mail/chat/code/terminal/notes). Everything else — browsers,
/// Finder, Electron wrappers, unknown — fails closed to skip the call.
/// Vague names never pass (checked first).
pub fn app_allows_context(app: &str) -> bool {
    if crate::cleanup::is_vague_app(app) {
        return false;
    }
    const ALLOW: &[&str] = &[
        // Mail.
        "mail",
        "gmail",
        "outlook",
        "thunderbird",
        "superhuman",
        "proton",
        // Chat.
        "slack",
        "discord",
        "teams",
        "telegram",
        "whatsapp",
        "signal",
        "messages",
        "imessage",
        "messenger",
        // Code / terminal.
        "code",
        "cursor",
        "zed",
        "xcode",
        "vim",
        "neovim",
        "emacs",
        "sublime",
        "terminal",
        "iterm",
        "ghostty",
        "alacritty",
        "kitty",
        "warp",
        // Notes / docs.
        "notes",
        "notion",
        "obsidian",
        "word",
        "pages",
        "onenote",
        "bear",
        "craft",
        "logseq",
    ];
    let l = app.to_ascii_lowercase();
    ALLOW.iter().any(|m| l.contains(m))
}

/// Full context gate (pure, key-injected for tests): existing
/// enabled/key rules AND transcript value AND app allowlist.
pub fn context_allowed(
    app_name: Option<&str>,
    analysis: &CleanupAnalysis,
    cleanup_enabled: bool,
    context_enabled: bool,
    cloud_key_present: bool,
) -> bool {
    cleanup_enabled
        && context_enabled
        && cloud_key_present
        && analysis.wants_context()
        && app_name.is_some_and(app_allows_context)
}

/// Score for `cleanup route=…` logs (tuning signal, not a threshold).
pub fn route_score(route: &CleanupRoute) -> u8 {
    match route {
        CleanupRoute::Direct(a) | CleanupRoute::Llm(a) => a.reasons.score(),
    }
}

/// Compact reason label for `cleanup route=…` logs (never transcript text).
pub fn route_reason(cfg: &crate::config::Config, route: &CleanupRoute) -> String {
    match route {
        CleanupRoute::Direct(_) if !cfg.cleanup_enabled => "cleanup_disabled".into(),
        CleanupRoute::Direct(a) if a.reasons.names().is_empty() => "already_clean".into(),
        CleanupRoute::Direct(a) | CleanupRoute::Llm(a) => a.reasons.names().join(","),
    }
}

/// Analyze + clean: runs the deterministic stages, then scores the result.
/// Detection runs on the CLEANED text (what would be injected), except the
/// hallucination-token check which reads the raw input (tokens are gone by
/// then). One lowercased copy serves all substring detectors; word-pair
/// detectors use case-insensitive comparison without allocating.
pub fn analyze_transcript(input: &str) -> CleanupAnalysis {
    let text = if input.is_empty() {
        String::new()
    } else {
        let stripped = crate::stt::strip_hallucination_tokens(input);
        let processed = crate::stt::post_process(&stripped);
        crate::stt::plus_to_symbol(&processed)
    };
    let reasons = detect_reasons(input, &text);
    let needs_llm = reasons.filler
        || reasons.repetition
        || reasons.self_correction
        || reasons.abandoned
        || reasons.number_words
        || reasons.spoken_punct
        || reasons.code_words
        || reasons.email_url
        || reasons.all_caps
        || reasons.punct_runs;
    CleanupAnalysis {
        changed: text != input,
        text,
        needs_llm,
        reasons,
    }
}

/// Alphanumeric core of a word, surrounding punctuation ignored
/// ("(A" → "A", "B," → "B"). Mirrors the math helper's contract.
fn core_word(w: &str) -> &str {
    let s = w.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    s.trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
}

fn detect_reasons(raw_input: &str, text: &str) -> CleanupReasons {
    let mut r = CleanupReasons::default();
    if text.trim().is_empty() {
        // Nothing to route (pure token input still records the removal).
        r.hallucination_removed = raw_input.contains('[');
        return r;
    }
    r.hallucination_removed =
        raw_input.len() != text.len() && raw_input.contains('[') && text.len() < raw_input.len();
    let lower = text.to_ascii_lowercase();

    let mut prev_core: Option<&str> = None;
    let mut last_core_lower = String::new();
    let mut caps_runs = 0u32;
    for w in text.split_whitespace() {
        let core = core_word(w);
        let core_lower = core.to_ascii_lowercase();
        last_core_lower.clear();
        last_core_lower.push_str(&core_lower);
        // Adjacent duplicates ("the the"), case-insensitive, punctuation
        // ignored ("hello, hello" counts — stutter either way).
        if !core.is_empty()
            && prev_core.is_some_and(|p| !p.is_empty() && p.eq_ignore_ascii_case(core))
        {
            r.repetition = true;
        }
        prev_core = Some(core);
        // Standalone fillers (exact token; "like" excluded — verb/prep FP).
        if matches!(core_lower.as_str(), "um" | "uh" | "eh" | "erm") {
            r.filler = true;
        }
        if core.len() >= 3 && core.bytes().all(|b| b.is_ascii_uppercase()) {
            caps_runs += 1;
        }
        if [
            "twenty",
            "thirty",
            "forty",
            "fifty",
            "sixty",
            "seventy",
            "eighty",
            "ninety",
            "thirteen",
            "fourteen",
            "fifteen",
            "sixteen",
            "seventeen",
            "eighteen",
            "nineteen",
            "hundred",
            "thousand",
            "million",
            "billion",
            "percent",
            "percentage",
            "dollar",
            "dollars",
            "euro",
            "euros",
            "dozen",
        ]
        .contains(&core_lower.as_str())
        {
            r.number_words = true;
        }
    }
    if caps_runs >= 2 {
        r.all_caps = true;
    }
    if lower.contains("you know") {
        r.filler = true;
    }
    if [
        "no actually",
        "i mean",
        "i meant",
        "sorry",
        "scratch that",
        "no perdón",
        "de fapt",
    ]
    .iter()
    .any(|p| lower.contains(p))
    {
        r.self_correction = true;
    }
    // Trailing filler / dangling correction.
    if matches!(last_core_lower.as_str(), "um" | "uh" | "eh" | "erm")
        || lower.ends_with("you know")
        || lower.ends_with("i mean")
    {
        r.abandoned = true;
    }
    if [
        "question mark",
        "exclamation mark",
        "exclamation point",
        "new line",
        "new paragraph",
        "bullet list",
        "numbered list",
    ]
    .iter()
    .any(|p| lower.contains(p))
    {
        r.spoken_punct = true;
    }
    if [
        "underscore",
        "snake case",
        "camel case",
        "kebab case",
        "dash dash",
    ]
    .iter()
    .any(|p| lower.contains(p))
    {
        r.code_words = true;
    }
    if ["@", "http", "www.", ".com", ".org", ".net", ".io"]
        .iter()
        .any(|p| lower.contains(p))
    {
        r.email_url = true;
    }
    if text.contains("..") || text.contains("!!") || text.contains("??") {
        r.punct_runs = true;
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn analyzed(input: &str) -> CleanupAnalysis {
        analyze_transcript(input)
    }

    #[test]
    fn test_clean_routes_deterministic() {
        for input in [
            "hello how are you",
            "hello world. this is wiflow.",
            "i think i can do it",
            "A plus B",
            "Playboy Carti and Rather Lie",
            "version 1.0 is ready. send it today.",
            "Good morning everyone. Thanks for joining, the migration is done and all checks are green.",
        ] {
            let a = analyzed(input);
            assert!(!a.needs_llm, "{input:?} → {a:?}");
            assert!(a.reasons.names().is_empty() || a.reasons.names() == ["hallucination_removed"]);
        }
    }

    #[test]
    fn test_complex_routes_llm_with_reasons() {
        let cases = [
            ("the the deployment is ready", vec!["repetition"]),
            ("um so I think we should deploy", vec!["filler"]),
            (
                "the deploy is Thursday no actually Wednesday",
                vec!["self_correction"],
            ),
            ("call me at twenty percent", vec!["number_words"]),
            ("send question mark", vec!["spoken_punct"]),
            ("rename to user underscore id", vec!["code_words"]),
            ("mail me at bob at example dot com", vec![]), // no '@' → deterministic
            ("mail bob@example.com now", vec!["email_url"]),
            ("HELLO WORLD MEETING", vec!["all_caps"]),
            ("wait what?? really", vec!["punct_runs"]),
            ("let's wrap up, so uh", vec!["abandoned"]),
        ];
        for (input, want) in cases {
            let a = analyzed(input);
            if want.is_empty() {
                assert!(!a.needs_llm, "{input:?} → {a:?}");
            } else {
                assert!(a.needs_llm, "{input:?} → {a:?}");
                for w in want {
                    assert!(a.reasons.names().contains(&w), "{input:?} → {a:?}");
                }
            }
        }
    }

    #[test]
    fn test_false_positive_guards() {
        // Ordinary words that merely resemble signals stay deterministic.
        for input in [
            "I like pie",                   // "like" is not filler here
            "the cost plus tax",            // prose, not math
            "Meet me at 3:30 PM",           // single acronym + time
            "the trial period ended",       // "period" is prose
            "café naïve déjà vu",           // non-ASCII preserved, no route
            "an API for масштаб",           // single acronym, mixed-language
            "one more thing",               // small number, no route
            "write a poem, comma optional", // hmm — "comma" is NOT a signal
        ] {
            let a = analyzed(input);
            assert!(!a.needs_llm, "{input:?} → {a:?}");
        }
    }

    #[test]
    fn test_hallucination_token_never_routes() {
        let a = analyzed("[BLANK_AUDIO]");
        assert_eq!(a.text, "");
        assert!(!a.needs_llm);
        assert!(a.reasons.hallucination_removed);
        let b = analyzed("hello [MUSIC] world");
        assert_eq!(b.text, "Hello world");
        assert!(!b.needs_llm);
        assert!(b.reasons.hallucination_removed);
    }

    #[test]
    fn test_changed_flag_and_score() {
        let a = analyzed("hello how are you");
        assert!(a.changed); // capitalization fixed deterministically
        assert_eq!(a.reasons.score(), 0);
        let b = analyzed("Hello how are you");
        assert!(!b.changed);
        assert!(!b.needs_llm);
        let c = analyzed("the deploy is Thursday no actually Wednesday");
        assert!(c.changed);
        assert!(c.reasons.score() >= 40);
    }

    #[test]
    fn bench_analyzer_overhead() {
        use std::hint::black_box;
        use std::time::Instant;
        for f in crate::baseline::FIXTURES {
            let iters = if f.input.len() > 120 { 300 } else { 2000 };
            for _ in 0..100 {
                black_box(analyze_transcript(black_box(f.input)));
            }
            let t0 = Instant::now();
            for _ in 0..iters {
                black_box(analyze_transcript(black_box(f.input)));
            }
            let ns = t0.elapsed().as_nanos() as u64 / iters;
            let a = analyze_transcript(f.input);
            eprintln!(
                "ANALYZER {:<16} ns/iter={:>6} needs_llm={} score={}",
                f.name,
                ns,
                a.needs_llm,
                a.reasons.score(),
            );
        }
    }

    #[test]
    fn test_deterministic_idempotent() {
        // The pipeline feeds STT output (already cleaned once) back through
        // the same stages on the Direct route — output must be a fixed point.
        for f in crate::baseline::FIXTURES {
            let once = crate::baseline::deterministic_clean(f.input);
            let twice = crate::baseline::deterministic_clean(&once);
            assert_eq!(once, twice, "not idempotent: {}", f.name);
        }
    }

    #[test]
    fn test_decide_route() {
        let on = crate::config::Config {
            cleanup_enabled: true,
            ..crate::config::Config::default()
        };
        let off = crate::config::Config {
            cleanup_enabled: false,
            ..crate::config::Config::default()
        };
        // Simple → direct inject, no LLM.
        match decide_route("hello how are you", &on) {
            CleanupRoute::Direct(a) => assert_eq!(a.text, "Hello how are you"),
            CleanupRoute::Llm(_) => panic!("simple must not route to LLM"),
        }
        // Complex → LLM chain.
        match decide_route("the deploy is Thursday no actually Wednesday", &on) {
            CleanupRoute::Llm(a) => {
                assert!(a.reasons.names().contains(&"self_correction"))
            }
            CleanupRoute::Direct(_) => panic!("self-correction must reach LLM"),
        }
        // Disabled → direct passthrough of whatever STT produced.
        match decide_route("um the the thing", &off) {
            CleanupRoute::Direct(a) => {
                assert_eq!(
                    a.text,
                    crate::baseline::deterministic_clean("um the the thing")
                )
            }
            CleanupRoute::Llm(_) => panic!("disabled cleanup must never route to LLM"),
        }
        // Empty → direct empty (downstream drops it, same as before).
        match decide_route("", &on) {
            CleanupRoute::Direct(a) => assert_eq!(a.text, ""),
            CleanupRoute::Llm(_) => panic!("empty must not route to LLM"),
        }
    }

    #[test]
    fn test_route_reason_labels() {
        let on = crate::config::Config {
            cleanup_enabled: true,
            ..crate::config::Config::default()
        };
        let off = crate::config::Config {
            cleanup_enabled: false,
            ..crate::config::Config::default()
        };
        assert_eq!(
            route_reason(&on, &decide_route("hello how are you", &on)),
            "already_clean"
        );
        assert_eq!(
            route_reason(
                &on,
                &decide_route("the deploy is Thursday no actually Wednesday", &on)
            ),
            "self_correction"
        );
        assert_eq!(
            route_reason(&on, &decide_route("um well the the thing", &on)),
            "filler,repetition"
        );
        assert_eq!(
            route_reason(&off, &decide_route("whatever", &off)),
            "cleanup_disabled"
        );
    }

    #[test]
    fn test_app_allowlist() {
        for app in [
            "Mail",
            "Gmail",
            "Slack",
            "Discord",
            "Code",
            "Terminal",
            "iTerm2",
            "Notes",
            "Notion",
            "Microsoft Outlook",
        ] {
            assert!(app_allows_context(app), "{app}");
        }
        for app in [
            "Safari",
            "Chrome",
            "Finder",
            "Electron",
            "Preview",
            "Unknown",
            "",
            "Some Random App",
        ] {
            assert!(!app_allows_context(app), "{app}");
        }
    }

    #[test]
    fn test_mid_sentence_capital() {
        assert!(analyze_transcript("hello Priya thanks").wants_context());
        assert!(analyze_transcript("see you Thursday").wants_context());
        assert!(analyze_transcript("mail bob@example.com").wants_context());
        assert!(analyze_transcript("rename to user underscore id").wants_context());
        for input in [
            "hello how are you",
            "Meet me at 3:30 PM",
            "I'm happy with it",
            "the the deployment is ready",
            "um so I think we should deploy",
            "Done. Yesterday was fine",
        ] {
            assert!(!analyze_transcript(input).wants_context(), "{input}");
        }
    }

    #[test]
    fn test_context_allowed_matrix() {
        let cfg_on = || crate::config::Config {
            cleanup_enabled: true,
            context_enabled: true,
            ..crate::config::Config::default()
        };
        let rich = analyze_transcript("hello Priya Nair thanks");
        let plain = analyze_transcript("hello how are you");
        // Full pass: enabled + key + allowlisted app + valuable transcript.
        assert!(context_allowed(Some("Mail"), &rich, true, true, true));
        // Each gate fails closed.
        assert!(!context_allowed(Some("Mail"), &plain, true, true, true)); // no value
        assert!(!context_allowed(Some("Safari"), &rich, true, true, true)); // app
        assert!(!context_allowed(None, &rich, true, true, true)); // no app
        assert!(!context_allowed(Some("Mail"), &rich, false, true, true)); // disabled
        assert!(!context_allowed(Some("Mail"), &rich, true, false, true)); // ctx off
        assert!(!context_allowed(Some("Mail"), &rich, true, true, false)); // no key
        assert!(!context_allowed(Some("unknown"), &rich, true, true, true)); // vague
        let _ = cfg_on;
    }

    #[test]
    fn test_corpus_context_calls() {
        // Context calls on the corpus (Mail app, cloud key): only transcripts
        // with spelling-sensitive content keep the second model call.
        let mut ctx = Vec::new();
        for f in crate::baseline::FIXTURES {
            if let CleanupRoute::Llm(a) = decide_route(
                f.input,
                &crate::config::Config {
                    cleanup_enabled: true,
                    ..crate::config::Config::default()
                },
            ) {
                if context_allowed(Some("Mail"), &a, true, true, true) {
                    ctx.push(f.name);
                }
            }
        }
        eprintln!("CONTEXT {ctx:?} (was every llm-routed call)");
        assert_eq!(ctx, vec!["self_correction"]);
    }

    /// Adversarial preservation: deterministic output keeps every byte it
    /// must keep; routing only escalates (never rewrites).
    #[test]
    fn test_adversarial_preservation() {
        // Decimals / versions / arithmetic lookalikes untouched.
        for (input, want) in [
            ("version 1.0 is live", "Version 1.0 is live"),
            ("version 1.2.3 released", "Version 1.2.3 released"),
            ("pi is 3.14 exactly", "Pi is 3.14 exactly"),
            ("the cost plus tax", "The cost plus tax"),
            ("1.0 plus 2.0", "1.0 plus 2.0"),
            ("A plus B plus C", "A+B+C"),
        ] {
            let a = analyze_transcript(input);
            assert_eq!(a.text, want, "{input}");
            assert!(!a.needs_llm, "{input} must not route: {a:?}");
        }
        // URLs / emails / markdown: bytes preserved; address-likes escalate.
        for (input, want, llm) in [
            (
                "see https://example.com/docs today",
                "See https://example.com/docs today",
                true,
            ),
            ("mail bob@example.com now", "Mail bob@example.com now", true),
            ("[ok] fine", "[ok] Fine", false),
            ("see [the docs] here", "See [the docs] here", false),
            ("[BLANK_AUDIO] [MUSIC]", "", false),
        ] {
            let a = analyze_transcript(input);
            assert_eq!(a.text, want, "{input}");
            assert_eq!(a.needs_llm, llm, "{input}");
        }
        // Code / identifiers / flags preserved verbatim (except sentence cap).
        for (input, want) in [
            ("use foo_bar in code", "Use foo_bar in code"),
            ("run with --fix flag", "Run with --fix flag"),
            ("set user_id now", "Set user_id now"),
            ("the api returns json", "The api returns json"),
        ] {
            let a = analyze_transcript(input);
            assert_eq!(a.text, want, "{input}");
            assert!(!a.needs_llm, "{input}");
        }
        // Contractions fixed, never routed.
        for (input, want) in [
            ("i'm happy", "I'm happy"),
            ("don't stop", "Don't stop"),
            ("it's fine i've got it", "It's fine I've got it"),
        ] {
            let a = analyze_transcript(input);
            assert_eq!(a.text, want, "{input}");
            assert!(!a.needs_llm, "{input}");
        }
        // Unicode / mixed-language: shape preserved, no escalation for mere
        // non-ASCII (deterministic already preserves it).
        for (input, want) in [
            ("café naïve déjà vu", "Café naïve déjà vu"),
            ("日本語のテストです", "日本語のテストです"),
            ("hello мир Café 123", "Hello мир Café 123"),
        ] {
            let a = analyze_transcript(input);
            assert_eq!(a.text, want, "{input}");
            assert!(!a.needs_llm, "{input}");
        }
        // Short / one-word / empty / punctuation-heavy.
        for (input, want, llm) in [
            ("hi", "Hi", false),
            ("ok", "Ok", false),
            ("a", "A", false),
            ("I", "I", false),
            ("", "", false),
            ("   ", "", false),
            ("wait... what?! no!!", "Wait... What?! No!!", true),
        ] {
            let a = analyze_transcript(input);
            assert_eq!(a.text, want, "{input:?}");
            assert_eq!(a.needs_llm, llm, "{input:?}");
        }
        // Documented escalation: "de fapt" is a real correction marker.
        let a = analyze_transcript("de fapt we should deploy");
        assert!(a.needs_llm);
        assert!(a.reasons.names().contains(&"self_correction"));
    }

    #[test]
    fn test_corpus_calls_avoided() {
        // Estimated LLM calls avoided on the Phase 1 corpus with cleanup on.
        let on = crate::config::Config {
            cleanup_enabled: true,
            ..crate::config::Config::default()
        };
        let mut direct = 0;
        let mut llm = 0;
        for f in crate::baseline::FIXTURES {
            match decide_route(f.input, &on) {
                CleanupRoute::Direct(_) => direct += 1,
                CleanupRoute::Llm(_) => llm += 1,
            }
        }
        eprintln!("ROUTE direct={direct}/12 llm={llm}/12 (was 11/12 llm)");
        assert_eq!((direct, llm), (9, 3));
    }

    #[test]
    fn test_fixture_corpus_routes() {
        // Phase 1 corpus through the analyzer: expect only genuinely complex
        // fixtures to need the LLM.
        let mut llm = Vec::new();
        for f in crate::baseline::FIXTURES {
            let a = analyzed(f.input);
            // Analyzer text must equal the deterministic baseline bytes.
            assert_eq!(a.text, crate::baseline::deterministic_clean(f.input));
            if a.needs_llm {
                llm.push((f.name, a.reasons.names()));
            }
        }
        eprintln!("ANALYZER llm_routes={llm:?}");
        assert_eq!(llm.len(), 3, "only repetition/filler/self_correction");
    }
}
