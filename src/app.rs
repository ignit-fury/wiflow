use tray_icon::Icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Task 2 wiring consumes AppState; tests-only until then.
pub enum AppState {
    Idle,
    Recording,
    Transcribing,
    Error,
}

#[allow(dead_code)] // Task 2 wiring consumes tooltip; tests-only until then.
impl AppState {
    pub fn tooltip(&self) -> &'static str {
        match self {
            AppState::Idle => "Wiflow — hold Right Option to dictate",
            AppState::Recording => "Wiflow — recording… release to transcribe",
            AppState::Transcribing => "Wiflow — transcribing…",
            AppState::Error => "Wiflow — error (see menu)",
        }
    }
}

/// 32x32 status icon: dark rounded square, center dot colored by state.
#[allow(dead_code)] // Task 2 wiring consumes icon_rgba; tests-only until then.
pub fn icon_rgba(state: AppState) -> Vec<u8> {
    let dot: (u8, u8, u8) = match state {
        AppState::Idle => (140, 140, 140),
        AppState::Recording => (230, 40, 40),
        AppState::Transcribing => (60, 180, 255),
        AppState::Error => (230, 150, 0),
    };
    let mut px = Vec::with_capacity(32 * 32 * 4);
    for y in 0..32i32 {
        for x in 0..32i32 {
            let dx = x - 16;
            let dy = y - 16;
            let (r, g, b) = if dx * dx + dy * dy <= 49 {
                dot
            } else {
                (24, 24, 24)
            };
            px.extend_from_slice(&[r, g, b, 255]);
        }
    }
    px
}

#[allow(dead_code)] // Task 2 wiring consumes make_icon; tests-only until then.
pub fn make_icon(state: AppState) -> Icon {
    Icon::from_rgba(icon_rgba(state), 32, 32).expect("generated icon is valid RGBA")
}

pub fn run() -> ! {
    eprintln!("tray shell lands in Task 2 wiring; --app parsed OK");
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tooltips_cover_all_states() {
        assert!(AppState::Idle.tooltip().contains("hold"));
        assert!(AppState::Recording.tooltip().contains("recording"));
        assert!(AppState::Transcribing.tooltip().contains("transcribing"));
        assert!(AppState::Error.tooltip().contains("error"));
    }

    #[test]
    fn test_icon_bytes_are_32x32_rgba() {
        for state in [
            AppState::Idle,
            AppState::Recording,
            AppState::Transcribing,
            AppState::Error,
        ] {
            let rgba = icon_rgba(state);
            assert_eq!(rgba.len(), 32 * 32 * 4);
        }
    }
    #[test]
    fn test_recording_dot_is_red_center() {
        let rgba = icon_rgba(AppState::Recording);
        let i = (16 * 32 + 16) * 4;
        assert!(
            rgba[i] > 200 && rgba[i + 1] < 80 && rgba[i + 2] < 80,
            "center must be red"
        );
    }

    #[test]
    fn test_each_state_has_distinct_center_dot() {
        let states = [
            AppState::Idle,
            AppState::Recording,
            AppState::Transcribing,
            AppState::Error,
        ];
        let mut dots = std::collections::HashSet::new();
        for s in states {
            let rgba = icon_rgba(s);
            let i = (16 * 32 + 16) * 4;
            dots.insert([rgba[i], rgba[i + 1], rgba[i + 2]]);
        }
        assert_eq!(dots.len(), states.len());
    }
}
