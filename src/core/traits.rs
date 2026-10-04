//! Core traits for the wiflow daemon pipeline.
//!
//! These traits define the interfaces for speech recognition, cleanup,
//! text injection, and context provision. Concrete implementations wrap
//! the existing functions (moved verbatim, zero behaviour change), enabling
//! testing with fakes — the seams S2/S4 tests build on.

use crate::core::cleanup::CleanupOutcome;
use crate::core::config::{Config, ModelChoice};

// ── Shared types ────────────────────────────────────────────────────────────

/// Transcript from STT: text plus optional provider warnings (e.g. cloud
/// fell back to local). The daemon maps each warning → CleanupIssue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
    pub warnings: Vec<String>,
}

/// Report from text injection into the focused cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectReport {
    pub pasted_via: &'static str,
    pub clipboard_restored: bool,
}

// ── SpeechRecognizer ────────────────────────────────────────────────────────

/// Speech-to-text: raw audio → transcript.
pub trait SpeechRecognizer {
    fn transcribe(
        &self,
        audio: &[f32],
        sample_rate: u32,
        cfg: &Config,
    ) -> Result<Transcript, String>;
}

/// Production: Groq cloud (opt-in) → local whisper fallback.
pub struct RouterRecognizer;

impl SpeechRecognizer for RouterRecognizer {
    fn transcribe(
        &self,
        audio: &[f32],
        sample_rate: u32,
        cfg: &Config,
    ) -> Result<Transcript, String> {
        let model_path = match cfg.model {
            ModelChoice::TinyEn => crate::core::stt::ensure_model_variant("tiny"),
            ModelChoice::SmallEn => crate::core::stt::ensure_model_variant("small"),
            ModelChoice::BaseEn => crate::core::stt::ensure_model_variant("base"),
        }?;
        let prompt = crate::core::stt::read_prompt();

        if cfg.stt_provider == "groq" {
            match crate::core::groq_stt::transcribe_cloud(audio, sample_rate, cfg) {
                Ok(t) => {
                    tracing::info!("cloud stt (whisper-large-v3) done");
                    Ok(Transcript {
                        text: t,
                        warnings: Vec::new(),
                    })
                }
                Err(e) => {
                    let msg = format!("Groq STT failed: {e} — using local whisper");
                    tracing::warn!("{msg}");
                    // Local fallback: warning surfaces at call site.
                    let text = crate::core::stt::transcribe_shared(
                        &model_path,
                        audio,
                        &prompt,
                        &cfg.stt_language,
                    )?;
                    Ok(Transcript {
                        text,
                        warnings: vec![msg],
                    })
                }
            }
        } else {
            let text = crate::core::stt::transcribe_shared(
                &model_path,
                audio,
                &prompt,
                &cfg.stt_language,
            )?;
            Ok(Transcript {
                text,
                warnings: Vec::new(),
            })
        }
    }
}

// ── CleanupProvider ─────────────────────────────────────────────────────────

/// Cleanup chain: route → (context + LLM / deterministic) → outcome.
pub trait CleanupProvider {
    fn clean(&self, text: &str, ctx: Option<&str>, cfg: &Config) -> CleanupOutcome;
}

/// Production: analyse → deterministic-clean or full LLM chain.
pub struct ChainProvider;

impl CleanupProvider for ChainProvider {
    fn clean(&self, text: &str, ctx: Option<&str>, cfg: &Config) -> CleanupOutcome {
        let route = crate::core::analyze::decide_route(text, cfg);
        let route_name = match &route {
            crate::core::analyze::CleanupRoute::Direct(_) => "deterministic",
            crate::core::analyze::CleanupRoute::Llm(_) => "llm",
        };
        tracing::info!(
            "cleanup route={route_name} reason={} score={}",
            crate::core::analyze::route_reason(cfg, &route),
            crate::core::analyze::route_score(&route),
        );
        match route {
            crate::core::analyze::CleanupRoute::Direct(a) => CleanupOutcome {
                text: a.text,
                issues: Vec::new(),
            },
            crate::core::analyze::CleanupRoute::Llm(_) => {
                let input = crate::core::cleanup::format_cleanup_input(ctx, text);
                crate::core::cleanup::clean_chain(&input, cfg)
            }
        }
    }
}

// ── TextInjector ────────────────────────────────────────────────────────────

/// Text injection into the focused cursor.
pub trait TextInjector {
    fn inject(&self, text: &str) -> Result<InjectReport, String>;
    fn leave_on_clipboard(&self, text: &str);
}

/// Production: clipboard save → set → Cmd+V → restore (enigo).
pub struct SystemInjector;

impl TextInjector for SystemInjector {
    fn inject(&self, text: &str) -> Result<InjectReport, String> {
        crate::platform::macos::inject::inject_text(text)
    }
    fn leave_on_clipboard(&self, text: &str) {
        crate::platform::macos::inject::leave_on_clipboard(text)
    }
}

// ── ContextProvider ─────────────────────────────────────────────────────────

/// Focused (frontmost) application name.
pub trait ContextProvider {
    fn focused_app(&self) -> Option<String>;
}

/// Production: osascript System Events query.
pub struct OsascriptContext;

impl ContextProvider for OsascriptContext {
    fn focused_app(&self) -> Option<String> {
        crate::platform::macos::context::focused_app_name()
    }
}

// ── MediaController ─────────────────────────────────────────────────────────

/// Competing-audio prioritization while the mic is hot: immediate volume
/// duck, then (after the gate) optional player pause — all owned here, never
/// in the orchestrator (H11). Implemented in `platform::macos::media`.
/// Wired in Task 12; unused until then.
#[allow(dead_code)]
pub trait MediaController {
    fn duck(&mut self);
    fn restore(&mut self);
}

// ── Tests (fakes are the seams S2/S4 build on) ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    // ── Fakes ───────────────────────────────────────────────────────────

    struct FakeRecognizer;
    impl SpeechRecognizer for FakeRecognizer {
        fn transcribe(
            &self,
            _audio: &[f32],
            _sample_rate: u32,
            _cfg: &Config,
        ) -> Result<Transcript, String> {
            Ok(Transcript {
                text: "fake transcript".to_string(),
                warnings: Vec::new(),
            })
        }
    }

    /// Fake that returns a transcript with a warning (simulates Groq→local
    /// fallback so the call-site warning-mapping path can be tested).
    struct FakeRecognizerWithWarning;
    impl SpeechRecognizer for FakeRecognizerWithWarning {
        fn transcribe(
            &self,
            _audio: &[f32],
            _sample_rate: u32,
            _cfg: &Config,
        ) -> Result<Transcript, String> {
            Ok(Transcript {
                text: "fake fallback transcript".to_string(),
                warnings: vec!["Groq STT failed: timeout — using local whisper".to_string()],
            })
        }
    }

    struct FakeCleaner;
    impl CleanupProvider for FakeCleaner {
        fn clean(&self, text: &str, _ctx: Option<&str>, _cfg: &Config) -> CleanupOutcome {
            CleanupOutcome {
                text: format!("cleaned: {text}"),
                issues: Vec::new(),
            }
        }
    }

    struct FakeInjectorFail {
        leave_called: Cell<bool>,
        left_text: RefCell<String>,
    }
    impl FakeInjectorFail {
        fn new() -> Self {
            Self {
                leave_called: Cell::new(false),
                left_text: RefCell::new(String::new()),
            }
        }
    }
    impl TextInjector for FakeInjectorFail {
        fn inject(&self, _text: &str) -> Result<InjectReport, String> {
            Err("inject failed".to_string())
        }
        fn leave_on_clipboard(&self, text: &str) {
            self.leave_called.set(true);
            *self.left_text.borrow_mut() = text.to_string();
        }
    }

    struct FakeInjectorOk;
    impl TextInjector for FakeInjectorOk {
        fn inject(&self, _text: &str) -> Result<InjectReport, String> {
            Ok(InjectReport {
                pasted_via: "fake",
                clipboard_restored: true,
            })
        }
        fn leave_on_clipboard(&self, _text: &str) {
            unreachable!("should not be called on success");
        }
    }

    struct FakeContext;
    impl ContextProvider for FakeContext {
        fn focused_app(&self) -> Option<String> {
            Some("TestApp".to_string())
        }
    }

    struct FakeContextNone;
    impl ContextProvider for FakeContextNone {
        fn focused_app(&self) -> Option<String> {
            None
        }
    }

    // ── Trait-contract tests ────────────────────────────────────────────

    #[test]
    fn test_fake_recognizer_text_flows() {
        let fake = FakeRecognizer;
        let cfg = Config::default();
        let result = fake.transcribe(&[0.0f32; 100], 16_000, &cfg);
        assert_eq!(
            result,
            Ok(Transcript {
                text: "fake transcript".to_string(),
                warnings: Vec::new(),
            })
        );
    }

    #[test]
    fn test_fake_recognizer_warning_surfaces() {
        // RED-first test: verifies the warning-mapping path the daemon
        // worker uses (each warning → CleanupIssue-equivalent).
        let fake = FakeRecognizerWithWarning;
        let cfg = Config::default();
        let transcript = fake
            .transcribe(&[0.0f32; 100], 16_000, &cfg)
            .expect("transcript must succeed after fallback");

        assert_eq!(transcript.text, "fake fallback transcript");
        assert_eq!(transcript.warnings.len(), 1);
        assert_eq!(
            transcript.warnings[0],
            "Groq STT failed: timeout — using local whisper"
        );

        // Simulate the daemon worker's warning→CleanupIssue mapping.
        let mut cleanup_issues = Vec::new();
        for w in &transcript.warnings {
            cleanup_issues.push(w.clone());
        }
        assert_eq!(
            cleanup_issues,
            vec!["Groq STT failed: timeout — using local whisper".to_string()]
        );
    }

    #[test]
    fn test_transcript_ok_no_warnings() {
        let fake = FakeRecognizer;
        let cfg = Config::default();
        let t = fake.transcribe(&[0.0], 16_000, &cfg).unwrap();
        assert!(t.warnings.is_empty());
    }

    #[test]
    fn test_fake_cleaner_returns_outcome() {
        let fake = FakeCleaner;
        let cfg = Config::default();
        let outcome = fake.clean("hello", None, &cfg);
        assert_eq!(outcome.text, "cleaned: hello");
        assert!(outcome.issues.is_empty());
    }

    #[test]
    fn test_fake_inject_fail_triggers_leave_on_clipboard() {
        let fake = FakeInjectorFail::new();
        let result = fake.inject("some text");
        assert!(result.is_err());
        // Simulate call-site fallback pattern
        if result.is_err() {
            fake.leave_on_clipboard("some text");
        }
        assert!(
            fake.leave_called.get(),
            "leave_on_clipboard must be called on inject failure"
        );
        assert_eq!(fake.left_text.borrow().as_str(), "some text");
    }

    #[test]
    fn test_fake_inject_ok_no_leave() {
        let fake = FakeInjectorOk;
        let result = fake.inject("hello");
        assert!(result.is_ok());
    }

    #[test]
    fn test_fake_context_returns_app() {
        let fake = FakeContext;
        assert_eq!(fake.focused_app(), Some("TestApp".to_string()));
    }

    #[test]
    fn test_fake_context_returns_none() {
        let fake = FakeContextNone;
        assert_eq!(fake.focused_app(), None);
    }

    // ── Real-impl smoke (verifies moved code compiles + basic wiring) ───

    #[test]
    fn test_chain_provider_direct_route() {
        // Simple text → deterministic route → cleaned text returned.
        let provider = ChainProvider;
        let cfg = Config::default();
        let outcome = provider.clean("hello how are you", None, &cfg);
        // Deterministic cleaning capitalises: "Hello how are you"
        assert_eq!(outcome.text, "Hello how are you");
        assert!(outcome.issues.is_empty());
    }

    #[test]
    fn test_chain_provider_llm_route_with_context() {
        // Self-correction triggers LLM route.
        let provider = ChainProvider;
        let cfg = Config {
            cleanup_provider: "ollama".into(),
            ..Config::default()
        };
        let ctx = "User is writing an email in Mail.";
        let outcome = provider.clean(
            "the deploy is Thursday no actually Wednesday",
            Some(ctx),
            &cfg,
        );
        // LLM route runs clean_chain; outcome.text must not be empty.
        assert!(
            !outcome.text.is_empty(),
            "LLM route must return non-empty text"
        );
    }

    #[test]
    fn test_system_injector_rejects_empty() {
        let injector = SystemInjector;
        assert!(injector.inject("").is_err());
        assert!(injector.inject("   ").is_err());
    }

    #[test]
    fn test_osascript_context_returns_app_or_none() {
        let ctx = OsascriptContext;
        // In CI/headless this may return None; on a desktop it returns Some.
        // Either is valid — the contract is Option<String>.
        let _ = ctx.focused_app();
    }

    // ── End-to-end fake pipeline (S2/S4 seam demo) ──────────────────────

    #[test]
    fn test_fake_pipeline_text_flows_through() {
        // Simulates: recognise → clean → inject (all fakes).
        let recognizer = FakeRecognizer;
        let cleaner = FakeCleaner;
        let injector = FakeInjectorOk;

        let cfg = Config::default();
        let transcript = recognizer.transcribe(&[0.0f32; 100], 16_000, &cfg).unwrap();
        assert_eq!(transcript.text, "fake transcript");
        assert!(transcript.warnings.is_empty());

        let outcome = cleaner.clean(&transcript.text, None, &cfg);
        assert_eq!(outcome.text, "cleaned: fake transcript");

        let report = injector.inject(&outcome.text);
        assert!(report.is_ok());
        assert_eq!(report.unwrap().pasted_via, "fake");
    }

    #[test]
    fn test_fake_pipeline_inject_fail_leaves_on_clipboard() {
        let recognizer = FakeRecognizer;
        let cleaner = FakeCleaner;
        let injector = FakeInjectorFail::new();

        let cfg = Config::default();
        let transcript = recognizer.transcribe(&[0.0f32; 100], 16_000, &cfg).unwrap();
        let outcome = cleaner.clean(&transcript.text, None, &cfg);

        let result = injector.inject(&outcome.text);
        if result.is_err() {
            injector.leave_on_clipboard(&outcome.text);
        }
        assert!(injector.leave_called.get());
        assert_eq!(
            injector.left_text.borrow().as_str(),
            "cleaned: fake transcript"
        );
    }

    #[test]
    fn test_fake_pipeline_with_warning_emits_cleanup_issue() {
        // Full fake pipeline: recognizer with warning → warning mapped to
        // CleanupIssue → clean → inject.
        let recognizer = FakeRecognizerWithWarning;
        let cleaner = FakeCleaner;
        let injector = FakeInjectorOk;

        let cfg = Config::default();
        let transcript = recognizer.transcribe(&[0.0f32; 100], 16_000, &cfg).unwrap();

        // Call-site warning → CleanupIssue mapping (same as daemon worker).
        let mut issues: Vec<String> = Vec::new();
        for w in &transcript.warnings {
            issues.push(w.clone());
        }
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("Groq STT failed"));

        let outcome = cleaner.clean(&transcript.text, None, &cfg);
        let report = injector.inject(&outcome.text);
        assert!(report.is_ok());
    }
}
