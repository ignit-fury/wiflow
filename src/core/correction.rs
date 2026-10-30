//! Conservative text correction. No model calls, fuzzy spelling, or lifecycle I/O.

use std::ops::Range;
use std::path::Path;

use serde::Deserialize;

const MAX_DICTIONARY_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WordCorrection {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(skip)]
    source_words: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Dictionary {
    entries: Vec<WordCorrection>,
}

impl Dictionary {
    pub fn load_from(path: &Path) -> Result<Self, String> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("read corrections: {e}")),
        };
        if file.metadata().map_err(|e| e.to_string())?.len() > MAX_DICTIONARY_BYTES {
            return Err("corrections.json exceeds 64 KiB".into());
        }
        // Bound the read as well as metadata (the file may change during a read).
        use std::io::Read;
        let mut json = String::new();
        file.take(MAX_DICTIONARY_BYTES + 1)
            .read_to_string(&mut json)
            .map_err(|e| e.to_string())?;
        if json.len() as u64 > MAX_DICTIONARY_BYTES {
            return Err("corrections.json exceeds 64 KiB".into());
        }
        Self::parse(&json)
    }

    fn parse(json: &str) -> Result<Self, String> {
        let mut entries: Vec<WordCorrection> =
            serde_json::from_str(json).map_err(|e| format!("invalid corrections.json: {e}"))?;
        if entries.len() > 256 {
            return Err("corrections.json exceeds 256 entries".into());
        }
        for entry in &mut entries {
            entry.source_words = entry.from.split_whitespace().map(str::to_string).collect();
            entry.from = entry.source_words.join(" ");
            entry.language = entry
                .language
                .take()
                .map(|language| language.trim().to_ascii_lowercase());
            entry.to = entry.to.trim().to_string();
            if entry.from.is_empty()
                || entry.to.is_empty()
                || entry.from.len() > 128
                || entry.to.len() > 128
                || entry
                    .from
                    .chars()
                    .any(|c| !(c.is_alphabetic() || c == ' ' || c == '\'' || c == '’'))
                || entry.to.chars().any(char::is_control)
                || entry
                    .language
                    .as_ref()
                    .is_some_and(|l| l.is_empty() || l == "auto")
            {
                return Err("correction entries need a plain-word source, a nonempty single-line replacement, and an optional explicit language".into());
            }
        }
        for (i, entry) in entries.iter().enumerate() {
            if entries[..i].iter().any(|other| {
                other.from.eq_ignore_ascii_case(&entry.from) && other.language == entry.language
            }) {
                return Err("corrections.json has duplicate sources for the same language".into());
            }
        }
        // Stable order: longest matching phrase wins. A language-scoped rule
        // takes precedence over an unscoped rule of equal length.
        entries.sort_by_key(|entry| {
            std::cmp::Reverse((
                entry.from.split_whitespace().count(),
                entry.language.is_some(),
            ))
        });
        Ok(Self { entries })
    }

    /// One pass over the original text: replacements never cascade.
    pub fn apply(&self, input: &str, language: &str) -> String {
        if self.entries.is_empty() || input.is_empty() {
            return input.to_string();
        }
        let ranges = protected_ranges(input);
        map_plain(input, &ranges, |plain| self.replace_plain(plain, language))
    }

    fn replace_plain(&self, input: &str, language: &str) -> String {
        let words = word_ranges(input);
        let mut out = String::with_capacity(input.len());
        let mut copied = 0;
        let mut i = 0;
        while i < words.len() {
            let matched = self.entries.iter().find_map(|entry| {
                if entry
                    .language
                    .as_deref()
                    .is_some_and(|l| !l.eq_ignore_ascii_case(language))
                {
                    return None;
                }
                let parts = &entry.source_words;
                if i + parts.len() > words.len() {
                    return None;
                }
                for (offset, part) in parts.iter().enumerate() {
                    let range = &words[i + offset];
                    if !input[range.clone()].eq_ignore_ascii_case(part) {
                        return None;
                    }
                    if offset > 0
                        && !input[words[i + offset - 1].end..range.start]
                            .chars()
                            .all(|c| c == ' ' || c == '\t')
                    {
                        return None;
                    }
                }
                Some((entry, parts.len()))
            });
            if let Some((entry, count)) = matched {
                out.push_str(&input[copied..words[i].start]);
                out.push_str(&entry.to);
                copied = words[i + count - 1].end;
                i += count;
            } else {
                i += 1;
            }
        }
        out.push_str(&input[copied..]);
        out
    }
}

pub fn effective_language(cfg: &crate::core::config::Config) -> &str {
    // All currently selectable local models are English-only (.en).
    // Cloud auto-detection does not give the cleanup layer a language code.
    if (cfg.stt_language == "auto" || cfg.stt_language.is_empty()) && cfg.stt_provider == "local" {
        "en"
    } else {
        &cfg.stt_language
    }
}

pub fn apply_saved_corrections(input: &str, cfg: &crate::core::config::Config) -> String {
    match Dictionary::load_from(&crate::core::config::corrections_path()) {
        Ok(dictionary) => dictionary.apply(input, effective_language(cfg)),
        Err(e) => {
            tracing::warn!("{e} — keeping recognized words");
            input.to_string()
        }
    }
}

/// Keep technical/quoted spans opaque to whitespace, capitalization and math.
pub fn deterministic_clean(input: &str, language: &str) -> String {
    let ranges = protected_ranges(input);
    let mut prefix = "WIFLOWOPAQUE".to_string();
    while input.contains(&prefix) {
        prefix.push('X');
    }
    let mut masked = String::with_capacity(input.len());
    let mut originals = Vec::new();
    let mut copied = 0;
    for range in ranges {
        masked.push_str(&input[copied..range.start]);
        let original = &input[range.clone()];
        let marker = if original.starts_with('[') {
            format!("[{prefix}{}TOKEN]", originals.len())
        } else {
            format!("{prefix}{}TOKEN", originals.len())
        };
        masked.push_str(&marker);
        originals.push((marker, original));
        copied = range.end;
    }
    masked.push_str(&input[copied..]);
    let stripped = crate::core::stt::strip_hallucination_tokens(&masked);
    let spaced = normalize_spaces(&stripped);
    let words = if language.eq_ignore_ascii_case("en") {
        clean_english_disfluencies(&spaced)
    } else {
        spaced
    };
    let processed =
        if language == "auto" || language.is_empty() || language.eq_ignore_ascii_case("en") {
            crate::core::stt::post_process(&words).into_owned()
        } else {
            words
        };
    // The existing math helper collapses line breaks. Preserve paragraph and
    // code layout, and only apply it to individual plain lines.
    let mut result = processed
        .split_inclusive('\n')
        .map(|line| {
            let (body, newline) = line
                .strip_suffix('\n')
                .map_or((line, ""), |body| (body, "\n"));
            format!("{}{newline}", crate::core::stt::plus_to_symbol(body))
        })
        .collect::<String>();
    for (marker, original) in originals {
        result = result.replace(&marker, original);
    }
    result
}

fn normalize_spaces(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut space = false;
    for c in input.trim().chars() {
        if c == ' ' || c == '\t' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

fn clean_english_disfluencies(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.split_inclusive('\n') {
        let mut previous = String::new();
        let mut have_word = false;
        for word in line.split_whitespace() {
            let core = word
                .trim_matches(|c: char| !c.is_alphabetic())
                .to_ascii_lowercase();
            // Never remove ordinary words like "like", "well", "so", or "you know".
            if matches!(core.as_str(), "um" | "uh" | "erm") {
                continue;
            }
            let duplicate = matches!(
                core.as_str(),
                "the" | "an" | "and" | "or" | "to" | "of" | "for" | "with"
            ) && previous.eq_ignore_ascii_case(word);
            if duplicate {
                continue;
            }
            if have_word {
                out.push(' ');
            }
            out.push_str(word);
            previous = word.to_string();
            have_word = true;
        }
        if line.ends_with('\n') {
            out.push('\n');
        }
    }
    out.trim().to_string()
}

fn word_ranges(input: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = None;
    for (i, c) in input.char_indices() {
        if c.is_alphabetic() || c == '\'' || c == '’' {
            start.get_or_insert(i);
        } else if let Some(start) = start.take() {
            ranges.push(start..i);
        }
    }
    if let Some(start) = start {
        ranges.push(start..input.len());
    }
    ranges
}

fn map_plain(input: &str, ranges: &[Range<usize>], mut f: impl FnMut(&str) -> String) -> String {
    let mut out = String::with_capacity(input.len());
    let mut copied = 0;
    for range in ranges {
        out.push_str(&f(&input[copied..range.start]));
        out.push_str(&input[range.clone()]);
        copied = range.end;
    }
    out.push_str(&f(&input[copied..]));
    out
}

/// Protected spans are also the contract checked after AI cleanup.
pub fn protected_ranges(input: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut offset = 0;
    for line in input.split_inclusive('\n') {
        let first = line.split_whitespace().next().unwrap_or("");
        if matches!(
            first,
            "git"
                | "cargo"
                | "npm"
                | "pnpm"
                | "yarn"
                | "python"
                | "python3"
                | "node"
                | "curl"
                | "sudo"
                | "rm"
                | "ls"
                | "cd"
                | "cp"
                | "mv"
                | "docker"
                | "kubectl"
                | "ssh"
                | "brew"
                | "rustc"
                | "sh"
                | "bash"
                | "zsh"
                | "pip"
                | "pip3"
        ) || line.contains(" = ")
            || line.contains("=>")
            || line.contains("->")
            || line.trim_end().ends_with(';')
        {
            ranges.push(offset..offset + line.trim_end_matches(['\n', '\r']).len());
        }
        offset += line.len();
    }
    let mut chars = input.char_indices().peekable();
    let mut previous = None;
    while let Some((start, c)) = chars.next() {
        let closing = match c {
            '`' => Some('`'),
            '"' | '“' => Some(if c == '“' { '”' } else { '"' }),
            '\'' | '‘' if !previous.is_some_and(char::is_alphanumeric) => {
                Some(if c == '‘' { '’' } else { '\'' })
            }
            '[' => Some(']'),
            '<' => Some('>'),
            _ => None,
        };
        if let Some(closing) = closing {
            let mut end = input.len();
            while let Some((pos, next)) = chars.next() {
                // An apostrophe inside "it's" is not the end of a quoted span.
                let contraction = matches!(closing, '\'' | '’')
                    && chars
                        .peek()
                        .is_some_and(|(_, following)| following.is_alphabetic());
                if next == closing && !contraction {
                    end = pos + next.len_utf8();
                    break;
                }
            }
            let span = &input[start..end];
            // Known Whisper noise annotations are removable outside quotes.
            if !matches!(
                span,
                "[BLANK_AUDIO]" | "[MUSIC]" | "[APPLAUSE]" | "[NOISE]" | "[SILENCE]"
            ) {
                ranges.push(start..end);
            }
            previous = Some(closing);
        } else {
            previous = Some(c);
        }
    }
    let mut offset = 0;
    for word in input.split_inclusive(char::is_whitespace) {
        let token = word.trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '(' | ')' | ',' | '.' | '!' | '?' | ';' | ':' | '"' | '“' | '”'
                )
        });
        let leading = word.find(token).unwrap_or(0);
        let mixed_case =
            token.chars().skip(1).any(char::is_uppercase) && token.chars().any(char::is_lowercase);
        let technical = token.contains(['/', '\\', '_', '@', '.', '-'])
            || token.chars().any(|c| c.is_ascii_digit())
            || mixed_case
            || (token.len() > 1 && token.chars().all(|c| c.is_ascii_uppercase()));
        if technical
            && !token.is_empty()
            && !matches!(
                token,
                "[BLANK_AUDIO]" | "[MUSIC]" | "[APPLAUSE]" | "[NOISE]" | "[SILENCE]"
            )
        {
            ranges.push(offset + leading..offset + leading + token.len());
        }
        offset += word.len();
    }
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::new();
    for range in ranges {
        if range.is_empty() {
            continue;
        }
        if let Some(last) = merged.last_mut() {
            if range.start <= last.end {
                last.end = last.end.max(range.end);
                continue;
            }
        }
        merged.push(range);
    }
    merged
}

/// Reject destructive cleanup; returning false keeps the deterministic result.
pub fn candidate_preserves_input(source: &str, candidate: &str) -> bool {
    if candidate.trim() == "EMPTY" || candidate.trim().is_empty() {
        return source.trim().is_empty();
    }
    let mut required = std::collections::HashMap::new();
    for range in protected_ranges(source) {
        *required.entry(&source[range]).or_insert(0usize) += 1;
    }
    if required
        .into_iter()
        .any(|(span, count)| candidate.matches(span).count() < count)
    {
        return false;
    }
    let source_words = canonical_words(source);
    let candidate_words = canonical_words(candidate);
    let mut allowed_numbers = std::collections::HashMap::new();
    for value in numeric_literals(source)
        .into_iter()
        .chain(spoken_numbers(&source_words))
    {
        *allowed_numbers.entry(value).or_insert(0usize) += 1;
    }
    for value in numeric_literals(candidate) {
        let Some(remaining) = allowed_numbers.get_mut(&value) else {
            return false;
        };
        if *remaining == 0 {
            return false;
        }
        *remaining -= 1;
    }
    let negations = |words: &[String]| {
        words
            .iter()
            .enumerate()
            .filter_map(|(i, word)| {
                // "no actually" / "no wait" mark a correction, not a negation.
                if word == "no"
                    && words
                        .get(i + 1)
                        .is_some_and(|next| next == "actually" || next == "wait")
                {
                    return None;
                }
                if word.ends_with("n't") || word.ends_with("n’t") || word == "cannot" {
                    Some("not".to_string())
                } else if matches!(word.as_str(), "not" | "never" | "no" | "without") {
                    Some(word.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    if negations(&source_words) != negations(&candidate_words) {
        return false;
    }
    // Added content words are a rewrite, not literal cleanup. Permit function
    // words for small grammar fixes, while protecting negation separately.
    let function_word = |word: &str| {
        matches!(
            word,
            "a" | "an"
                | "the"
                | "is"
                | "are"
                | "was"
                | "were"
                | "am"
                | "be"
                | "been"
                | "to"
                | "of"
                | "at"
                | "in"
                | "on"
                | "and"
                | "or"
                | "do"
                | "does"
                | "did"
        )
    };
    if candidate_words
        .iter()
        .any(|word| !function_word(word) && !source_words.contains(word))
    {
        return false;
    }
    // Without an explicit self-correction, AI must retain substantive words.
    // Formatting commands and disfluencies may disappear, ordinary content may not.
    let mut formatting = vec![false; source_words.len()];
    for pattern in [
        &["question", "mark"][..],
        &["exclamation", "mark"],
        &["exclamation", "point"],
        &["new", "line"],
        &["new", "paragraph"],
        &["snake", "case"],
        &["camel", "case"],
        &["dash", "dash"],
        &["at", "sign"],
    ] {
        for (i, words) in source_words.windows(pattern.len()).enumerate() {
            if words.iter().map(String::as_str).eq(pattern.iter().copied()) {
                let prose_new = pattern[0] == "new"
                    && i > 0
                    && matches!(
                        source_words[i - 1].as_str(),
                        "a" | "the" | "this" | "that" | "another" | "add" | "create" | "start"
                    );
                if !prose_new {
                    formatting[i..i + pattern.len()].fill(true);
                }
            }
        }
    }
    for (i, word) in source_words.iter().enumerate() {
        if matches!(
            word.as_str(),
            "um" | "uh" | "erm" | "underscore" | "comma" | "period" | "dot" | "slash"
        ) || (matches!(word.as_str(), "percent" | "percentage") && candidate.contains('%'))
            || (matches!(word.as_str(), "dollar" | "dollars") && candidate.contains('$'))
        {
            formatting[i] = true;
        }
    }
    let number_word =
        |word: &str| cardinal(word).is_some() || matches!(word, "hundred" | "thousand" | "million");
    let content = |word: &str| !function_word(word) && !number_word(word);
    let mut last_correction = None;
    for (i, words) in source_words.windows(2).enumerate() {
        if matches!(
            (words[0].as_str(), words[1].as_str()),
            ("no", "actually")
                | ("i", "mean")
                | ("i", "meant")
                | ("scratch", "that")
                | ("no", "wait")
                | ("wait", "no")
                | ("or", "rather")
        ) {
            last_correction = Some(i + 2);
        } else if words[0] == "actually" && words[1] == "make" {
            // "actually make it Thursday": the replacement starts after "it".
            let skip = source_words
                .get(i + 2)
                .is_some_and(|w| w == "it" || w == "that");
            last_correction = Some(i + 2 + usize::from(skip));
        }
    }
    if let Some(i) = source_words.iter().rposition(|word| word == "sorry") {
        last_correction =
            Some(last_correction.map_or(i + 1, |previous: usize| previous.max(i + 1)));
    }
    // Require the final replacement phrase in full, including the end of a
    // long dictation; a token cap must not quietly truncate the tail.
    let retain_from = last_correction.unwrap_or(0);
    if source_words
        .iter()
        .enumerate()
        .skip(retain_from)
        .any(|(i, word)| !formatting[i] && content(word) && !candidate_words.contains(word))
    {
        return false;
    }
    // Spelled numbers in the replacement must survive too: "fifty thousand
    // scratch that sixty thousand" must not come back as "fifty thousand".
    if last_correction.is_some() {
        let mut kept = spoken_numbers(&candidate_words);
        kept.extend(numeric_literals(candidate));
        if spoken_numbers(&source_words[retain_from.min(source_words.len())..])
            .iter()
            .any(|value| !kept.contains(value))
        {
            return false;
        }
    }
    // These repetitions have valid grammar; punctuation cleanup must not erase them.
    for word in ["had", "that"] {
        if source_words
            .windows(2)
            .any(|pair| pair[0] == word && pair[1] == word)
            && candidate_words
                .iter()
                .filter(|candidate| candidate.as_str() == word)
                .count()
                < source_words
                    .iter()
                    .filter(|source| source.as_str() == word)
                    .count()
        {
            return false;
        }
    }
    let before = source_words
        .iter()
        .enumerate()
        .filter(|(i, word)| !formatting[*i] && content(word))
        .count();
    let after = candidate_words.iter().filter(|word| content(word)).count();
    if before > 0 && after == 0 {
        return false;
    }
    if before >= 8 && after * 2 < before {
        return false;
    }
    true
}

fn canonical_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    for range in word_ranges(text) {
        let word = text[range].to_lowercase().replace('’', "'");
        let expanded = match word.as_str() {
            "can't" | "cannot" => "can not",
            "won't" => "will not",
            "shan't" => "shall not",
            "i'm" => "i am",
            "you're" => "you are",
            "we're" => "we are",
            "they're" => "they are",
            "it's" => "it is",
            "he's" => "he is",
            "she's" => "she is",
            "that's" => "that is",
            "i've" => "i have",
            "we've" => "we have",
            "you've" => "you have",
            "they've" => "they have",
            "i'll" => "i will",
            "we'll" => "we will",
            "you'll" => "you will",
            "they'll" => "they will",
            _ => "",
        };
        if !expanded.is_empty() {
            words.extend(expanded.split_whitespace().map(str::to_string));
        } else if let Some(base) = word.strip_suffix("n't") {
            words.push(base.to_string());
            words.push("not".into());
        } else {
            words.push(word);
        }
    }
    words
}

fn numeric_literals(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().chain(std::iter::once(' ')).collect();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_digit() {
            current.push(c);
        } else if c == ','
            && !current.is_empty()
            && chars.get(i + 1).is_some_and(char::is_ascii_digit)
        {
            // Digit grouping: "60,000" is one number.
        } else if !current.is_empty() {
            values.push(std::mem::take(&mut current));
        }
    }
    values
}

fn cardinal(word: &str) -> Option<u64> {
    match word {
        "zero" => Some(0),
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        "thirteen" => Some(13),
        "fourteen" => Some(14),
        "fifteen" => Some(15),
        "sixteen" => Some(16),
        "seventeen" => Some(17),
        "eighteen" => Some(18),
        "nineteen" => Some(19),
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "fifty" => Some(50),
        "sixty" => Some(60),
        "seventy" => Some(70),
        "eighty" => Some(80),
        "ninety" => Some(90),
        _ => None,
    }
}

fn spoken_numbers(words: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let Some(mut value) = cardinal(&words[i]) else {
            i += 1;
            continue;
        };
        i += 1;
        if value >= 20 && value % 10 == 0 {
            if let Some(unit) = words
                .get(i)
                .and_then(|word| cardinal(word))
                .filter(|n| *n < 10)
            {
                value += unit;
                i += 1;
            }
        }
        if words.get(i).is_some_and(|word| word == "hundred") {
            value *= 100;
            i += 1;
            if words.get(i).is_some_and(|word| word == "and") {
                i += 1;
            }
            if let Some(rest) = words.get(i).and_then(|word| cardinal(word)) {
                value += rest;
                i += 1;
                if rest >= 20 && rest % 10 == 0 {
                    if let Some(unit) = words
                        .get(i)
                        .and_then(|word| cardinal(word))
                        .filter(|n| *n < 10)
                    {
                        value += unit;
                        i += 1;
                    }
                }
            }
        }
        if let Some(scale) = words.get(i).and_then(|word| match word.as_str() {
            "thousand" => Some(1000),
            "million" => Some(1_000_000),
            _ => None,
        }) {
            value *= scale;
            i += 1;
        }
        values.push(value.to_string());
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn correction_and_preservation_corpus() {
        let cases = [
            ("um  the the deployment is ready", "The deployment is ready"),
            ("uh, I think we should deploy.", "I think we should deploy."),
            ("this is an an example", "This is an example"),
            ("well, I like pie.", "Well, I like pie."),
            ("I had had enough.", "I had had enough."),
            (
                "I know that that idea works.",
                "I know that that idea works.",
            ),
            (
                "the, the deployment is ready",
                "The, the deployment is ready",
            ),
            (
                "let's meet Thursday no actually Wednesday",
                "Let's meet Thursday no actually Wednesday",
            ),
            ("first line.\n\nsecond line.", "First line.\n\nSecond line."),
            ("um uh erm", ""),
            ("A plus B", "A+B"),
            ("1.0 plus 2.0", "1.0 plus 2.0"),
            (
                "send to bob@example.com at 3:30 PM",
                "Send to bob@example.com at 3:30 PM",
            ),
            (
                "open https://example.com/api/v2",
                "Open https://example.com/api/v2",
            ),
            ("foo_bar must stay lowercase", "foo_bar must stay lowercase"),
            ("fooBar must stay mixed case", "fooBar must stay mixed case"),
            (
                "git commit --message \"um the the\"",
                "git commit --message \"um the the\"",
            ),
            (
                "  cargo test --offline\n  git status",
                "  cargo test --offline\n  git status",
            ),
            ("`let user_id = 2;`", "`let user_id = 2;`"),
            ("user_id = \"um\";", "user_id = \"um\";"),
            ("\"um the the\" stays quoted", "\"um the the\" stays quoted"),
            (
                "'it's um the the' stays quoted",
                "'it's um the the' stays quoted",
            ),
            ("keep [API] and [HTTP]", "Keep [API] and [HTTP]"),
            ("[BLANK_AUDIO] [MUSIC]", ""),
            ("say \"[MUSIC]\" literally", "Say \"[MUSIC]\" literally"),
        ];
        for (input, expected) in cases {
            let actual = deterministic_clean(input, "en");
            assert_eq!(actual, expected, "{input:?}");
            assert_eq!(
                deterministic_clean(&actual, "en"),
                actual,
                "idempotence: {input:?}"
            );
        }
    }

    #[test]
    fn english_rules_do_not_guess_cloud_language() {
        let local = crate::core::config::Config::default();
        assert_eq!(effective_language(&local), "en");
        let cloud = crate::core::config::Config {
            stt_provider: "groq".into(),
            ..local
        };
        assert_eq!(effective_language(&cloud), "auto");
        assert_eq!(deterministic_clean("um pedido", "pt"), "um pedido");
        assert_eq!(deterministic_clean("a i b", "pl"), "a i b");
        assert_eq!(deterministic_clean("um pedido", "auto"), "Um pedido");
        assert_eq!(deterministic_clean("हिंदी में बात करें", "hi"), "हिंदी में बात करें");
    }

    #[test]
    fn dictionary_matches_whole_phrases_without_cascading() {
        let dictionary = Dictionary::parse(
            r#"[
            {"from":"v flow","to":"Wiflow"},
            {"from":"flow","to":"stream"},
            {"from":"Wiflow","to":"another product"},
            {"from":"sentance","to":"sentence","language":"en"}
        ]"#,
        )
        .unwrap();
        assert_eq!(
            dictionary.apply("V flow, flow and flowing.", "en"),
            "Wiflow, stream and flowing."
        );
        assert_eq!(dictionary.apply("v, flow", "en"), "v, stream");
        assert_eq!(dictionary.apply("v\nflow", "en"), "v\nstream");
        assert_eq!(dictionary.apply("fix sentance", "en"), "fix sentence");
        assert_eq!(dictionary.apply("fix sentance", "auto"), "fix sentance");
        assert_eq!(dictionary.apply("fix sentance", "hi"), "fix sentance");
    }

    #[test]
    fn dictionary_preserves_technical_and_quoted_content() {
        let dictionary = Dictionary::parse(r#"[{"from":"flow","to":"stream"}]"#).unwrap();
        for input in [
            "flow_id",
            "flow.js",
            "https://flow.example",
            "flow@example.com",
            "flow2",
            "flowState",
            "`flow`",
            "\"flow\"",
            "'flow'",
            "git checkout flow",
            "[flow]",
            "<flow>",
        ] {
            assert_eq!(dictionary.apply(input, "en"), input, "{input}");
        }
    }

    #[test]
    fn explicit_language_overrides_global_mapping() {
        let dictionary = Dictionary::parse(
            r#"[
            {"from":"flow","to":"global"},
            {"from":"flow","to":"English","language":"EN"}
        ]"#,
        )
        .unwrap();
        assert_eq!(dictionary.apply("flow", "en"), "English");
        assert_eq!(dictionary.apply("flow", "auto"), "global");
    }

    #[test]
    fn malformed_dictionary_fails_without_modifying_transcript() {
        for json in [
            "not json",
            r#"[{"from":"","to":"foo"}]"#,
            r#"[{"from":"foo","to":""}]"#,
            r#"[{"from":"foo_bar","to":"bar"}]"#,
            r#"[{"from":"foo","to":"bar","language":"auto"}]"#,
            r#"[{"from":"foo","to":"bar"},{"from":"FOO","to":"baz"}]"#,
        ] {
            assert!(Dictionary::parse(json).is_err(), "{json}");
        }
        let path =
            std::env::temp_dir().join(format!("wiflow-dictionary-{}.json", std::process::id()));
        assert_eq!(
            Dictionary::load_from(&path)
                .unwrap()
                .apply("hello world", "en"),
            "hello world"
        );
        std::fs::write(&path, "invalid json").unwrap();
        assert!(Dictionary::load_from(&path).is_err());
        std::fs::write(&path, vec![b' '; MAX_DICTIONARY_BYTES as usize + 1]).unwrap();
        assert!(Dictionary::load_from(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn ai_accepts_literal_sentence_cleanup() {
        for (source, candidate) in [
            (
                "Let's meet Thursday no actually Wednesday",
                "Let's meet Wednesday.",
            ),
            ("We was ready", "We were ready."),
            ("I do not want to deploy", "I don't want to deploy."),
            ("Call me at twenty percent", "Call me at 20%."),
            ("Meet me at three thirty pm", "Meet me at 3:30 pm."),
            (
                "The count is one hundred and twenty two",
                "The count is 122.",
            ),
            ("Rename to user underscore id", "Rename to user_id."),
            ("Send question mark", "Send?"),
            ("The migration is ready", "The migration is ready."),
            (
                "Let's use Postgres no wait MySQL for the new service",
                "Let's use MySQL for the new service.",
            ),
            (
                "Call me tomorrow, actually make it Thursday, around noon",
                "Call me Thursday around noon.",
            ),
            (
                "The budget is fifty thousand scratch that sixty thousand dollars",
                "The budget is $60,000.",
            ),
            (
                "The budget is fifty thousand scratch that sixty thousand dollars",
                "The budget is sixty thousand dollars.",
            ),
        ] {
            assert!(
                candidate_preserves_input(source, candidate),
                "{source:?} → {candidate:?}"
            );
        }
    }

    #[test]
    fn ai_rejects_changed_numbers_negation_code_and_unrelated_text() {
        for (source, candidate) in [
            ("Send 25 dollars", "Send 250 dollars."),
            ("Send twenty dollars", "Send 30 dollars."),
            ("Send the report", "Send the report in 2027."),
            ("Do not deploy", "Do deploy."),
            ("Never deploy", "Deploy."),
            ("Deploy without restarting", "Deploy after restarting."),
            ("Rename user_id", "Rename userId."),
            ("Use https://example.com/v2", "Use https://example.com/v3."),
            ("Use fooBar", "Use FooBar."),
            ("git status --short", "Git status --short"),
            ("I like apples", "I hate apples."),
            ("I like apples", "I like."),
            ("The deployment is ready", "EMPTY"),
            ("We will review the migration tomorrow", ""),
            ("We will review the migration tomorrow", "We will review."),
            (
                "The budget is fifty thousand scratch that sixty thousand dollars",
                "The budget is fifty thousand dollars.",
            ),
            (
                "Let's use Postgres no wait MySQL for the new service",
                "Let's use Postgres for the new service.",
            ),
            // llama3.2:3b outputs seen in testing (2026-10-30).
            ("there is no waiting list today so just come in", "come in"),
            (
                "call the dentist on Wednesday no wait Thursday at noon",
                "Let's meet Wednesday at noon.",
            ),
            (
                "please review the pull request and merge it if the tests pass",
                "I'm ready to review the pull request. Please go ahead and provide the raw speech transcript.",
            ),
        ] {
            assert!(
                !candidate_preserves_input(source, candidate),
                "{source:?} → {candidate:?}"
            );
        }
    }

    #[test]
    fn corrections_reduce_calls_without_losing_ambiguous_markers() {
        let cfg = crate::core::config::Config::default();
        assert!(matches!(
            crate::core::analyze::decide_route("um the the build is ready", &cfg),
            crate::core::analyze::CleanupRoute::Direct(_)
        ));
        assert!(matches!(
            crate::core::analyze::decide_route("Thursday no actually Wednesday", &cfg),
            crate::core::analyze::CleanupRoute::Llm(_)
        ));
        assert!(matches!(
            crate::core::analyze::decide_route("I had had enough", &cfg),
            crate::core::analyze::CleanupRoute::Llm(_)
        ));
    }

    #[test]
    fn benchmark_correction_corpus() {
        use std::hint::black_box;
        let input =
            "um the the build is ready. Rename foo_bar and open https://example.com/v2 at 3:30 PM.";
        let started = std::time::Instant::now();
        for _ in 0..1000 {
            black_box(deterministic_clean(black_box(input), "en"));
        }
        eprintln!(
            "CORRECTION deterministic mean={}µs (1000 iterations)",
            started.elapsed().as_micros() / 1000
        );
    }
}
