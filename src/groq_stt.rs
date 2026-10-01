/// Encode mono f32 samples to a 16-bit WAV in memory (pure, tested).
pub fn encode_wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::new(&mut cursor, spec).expect("wav writer");
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .expect("sample");
    }
    w.finalize().expect("finalize");
    cursor.into_inner()
}

/// Build a multipart/form-data body for Groq audio transcription (pure, tested).
pub fn build_multipart(boundary: &str, wav: &[u8], model: &str, language: Option<&str>) -> Vec<u8> {
    let mut body = Vec::new();
    let mut part = |field: &str, value: &str| {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    };
    part("model", model);
    if let Some(lang) = language {
        part("language", lang);
    }
    part("response_format", "json");
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\nContent-Type: audio/wav\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(wav);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}

/// Extract the `text` field from a Groq transcription JSON response.
pub fn parse_transcript(body: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()?
        .get("text")?
        .as_str()
        .map(|s| s.trim().to_string())
}

/// Cloud STT via Groq whisper-large-v3 (OPT-IN: audio leaves the device only
/// when stt_provider == "groq" — privacy rule).
pub fn transcribe_cloud(
    samples: &[f32],
    rate: u32,
    cfg: &crate::config::Config,
) -> Result<String, String> {
    let key = crate::cleanup::groq_key().ok_or("no Groq key (set GROQ_API_KEY)")?;
    let wav = encode_wav16(samples, rate);
    let boundary = "wiflow-audio-boundary-7f3a";
    let body = build_multipart(
        boundary,
        &wav,
        "whisper-large-v3",
        lang_opt(&cfg.stt_language),
    );
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .build();
    let resp = agent
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .set("Authorization", &format!("Bearer {key}"))
        .set(
            "Content-Type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .send_bytes(&body)
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            other => format!("{other}"),
        })?;
    let text_raw = resp.into_string().map_err(|e| format!("read: {e:?}"))?;
    parse_transcript(&text_raw).ok_or_else(|| format!("unparseable: {}", text_raw))
}

/// "auto"/empty → None (cloud auto-detect); an ISO code passes through.
fn lang_opt(lang: &str) -> Option<&str> {
    if lang.is_empty() || lang == "auto" {
        None
    } else {
        Some(lang)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_wav16_shape() {
        let samples = vec![0.0f32, 0.5, -0.5, 1.0];
        let wav = encode_wav16(&samples, 16_000);
        assert!(wav.len() > 44, "must include RIFF header");
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
    }

    #[test]
    fn test_build_multipart_shape() {
        let wav = vec![1u8, 2, 3];
        let body = build_multipart("BOUNDARY123", &wav, "whisper-large-v3", None);
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("--BOUNDARY123"));
        assert!(s.contains("name=\"model\""));
        assert!(s.contains("whisper-large-v3"));
        assert!(s.contains("name=\"file\"; filename=\"audio.wav\""));
        assert!(s.contains("Content-Type: audio/wav"));
        assert!(s.ends_with("--BOUNDARY123--\r\n"));
    }

    #[test]
    fn test_build_multipart_with_language() {
        let body = build_multipart("B", &[1u8], "whisper-large-v3", Some("en"));
        let s = String::from_utf8_lossy(&body);
        assert!(s.contains("name=\"language\""));
        assert!(s.contains("\r\nen\r\n"));
        let auto = build_multipart("B", &[1u8], "whisper-large-v3", None);
        assert!(!String::from_utf8_lossy(&auto).contains("name=\"language\""));
    }

    #[test]
    fn test_parse_transcript() {
        assert_eq!(
            parse_transcript(r#"{"text":"Hello world."}"#),
            Some("Hello world.".into())
        );
        assert_eq!(parse_transcript("not json"), None);
    }

    #[test]
    fn test_lang_opt() {
        assert_eq!(lang_opt("auto"), None);
        assert_eq!(lang_opt(""), None);
        assert_eq!(lang_opt("en"), Some("en"));
    }
}
