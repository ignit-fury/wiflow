// Phase 1: PushToTalk/PttEvent consumed by Task 4 wiring.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PttEvent {
    Started,
    IgnoredRepeat,
    Ignored,
    Transcribe {
        duration_ms: u64,
    },
    DiscardedShort {
        duration_ms: u64,
    },
    #[allow(dead_code)]
    Cancelled,
}

#[derive(Debug)]
pub struct PushToTalk {
    min_ms: u64,
    max_ms: u64,
    down_at: Option<u64>,
}

impl PushToTalk {
    pub fn new(min_ms: u64, max_ms: u64) -> Self {
        Self {
            min_ms,
            max_ms,
            down_at: None,
        }
    }

    #[allow(dead_code)]
    pub fn is_recording(&self) -> bool {
        self.down_at.is_some()
    }

    pub fn on_key_down(&mut self, now_ms: u64) -> PttEvent {
        if self.down_at.is_some() {
            return PttEvent::IgnoredRepeat;
        }
        self.down_at = Some(now_ms);
        PttEvent::Started
    }

    pub fn on_key_up(&mut self, now_ms: u64) -> PttEvent {
        let start = match self.down_at.take() {
            Some(t) => t,
            None => return PttEvent::Ignored,
        };
        let raw = now_ms.saturating_sub(start);
        if raw < self.min_ms {
            return PttEvent::DiscardedShort { duration_ms: raw };
        }
        PttEvent::Transcribe {
            duration_ms: raw.min(self.max_ms),
        }
    }

    #[allow(dead_code)]
    pub fn on_cancel(&mut self) -> PttEvent {
        if self.down_at.take().is_some() {
            PttEvent::Cancelled
        } else {
            PttEvent::Ignored
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short_tap_discarded() {
        let mut p = PushToTalk::new(300, 60_000);
        assert!(matches!(p.on_key_down(1000), PttEvent::Started));
        assert!(matches!(p.on_key_up(1100), PttEvent::DiscardedShort { .. }));
    }

    #[test]
    fn test_normal_hold_transcribes() {
        let mut p = PushToTalk::new(300, 60_000);
        p.on_key_down(0);
        match p.on_key_up(2500) {
            PttEvent::Transcribe { duration_ms } => assert_eq!(duration_ms, 2500),
            e => panic!("expected Transcribe, got {:?}", e),
        }
    }

    #[test]
    fn test_repeat_keydown_ignored() {
        let mut p = PushToTalk::new(300, 60_000);
        assert!(matches!(p.on_key_down(0), PttEvent::Started));
        assert!(matches!(p.on_key_down(10), PttEvent::IgnoredRepeat));
    }

    #[test]
    fn test_cancel() {
        let mut p = PushToTalk::new(300, 60_000);
        p.on_key_down(0);
        assert!(matches!(p.on_cancel(), PttEvent::Cancelled));
        assert!(matches!(p.on_key_up(5000), PttEvent::Ignored));
    }

    #[test]
    fn test_auto_stop_at_max() {
        let mut p = PushToTalk::new(300, 60_000);
        p.on_key_down(0);
        match p.on_key_up(61_000) {
            PttEvent::Transcribe { duration_ms } => assert_eq!(duration_ms, 60_000),
            e => panic!("expected clamped Transcribe, got {:?}", e),
        }
    }

    #[test]
    fn test_stray_key_up_ignored() {
        let mut p = PushToTalk::new(300, 60_000);
        assert!(matches!(p.on_key_up(5000), PttEvent::Ignored));
    }
}
