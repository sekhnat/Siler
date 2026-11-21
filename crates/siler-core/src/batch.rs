//! Numbered batch downloads: one address pattern expands to many addresses, like IDM's
//! "Add batch download" and curl's globbing.
//!
//! - `[1-100]` counts, `[001-100]` keeps the zeros (the width of the first number),
//! - `[a-z]` and `[A-Z]` go through letters,
//! - `[0-100:5]` counts in steps,
//! - `{jpg,png,gif}` takes each word in turn.
//!
//! Several groups combine: `img[1-3]_{a,b}.jpg` gives six addresses. A bracket that isn't a
//! range (an IPv6 address such as `[::1]`) is kept as it is; `\[` and `\{` are literal.

/// The most addresses one pattern may give.
pub const MAX_URLS: usize = 10_000;

enum Part {
    Lit(String),
    Choices(Vec<String>),
}

/// Expands `pattern` into its addresses, in order (the last group changes fastest).
pub fn expand(pattern: &str) -> Result<Vec<String>, String> {
    let parts = parse(pattern.trim())?;
    let mut total: usize = 1;
    let mut groups = 0;
    for p in &parts {
        if let Part::Choices(c) = p {
            groups += 1;
            total = total.saturating_mul(c.len());
        }
    }
    if groups == 0 {
        return Err(
            "The address has no range like [1-10] or list like {a,b}, so there's nothing to number."
                .into(),
        );
    }
    if total > MAX_URLS {
        return Err(format!(
            "This gives {} addresses; Siler takes at most {} at a time.",
            if total == usize::MAX {
                "too many".to_string()
            } else {
                total.to_string()
            },
            MAX_URLS
        ));
    }
    let mut out = vec![String::new()];
    for p in &parts {
        match p {
            Part::Lit(s) => out.iter_mut().for_each(|o| o.push_str(s)),
            Part::Choices(c) => {
                out = out
                    .iter()
                    .flat_map(|o| c.iter().map(move |x| format!("{o}{x}")))
                    .collect();
            }
        }
    }
    Ok(out)
}

fn parse(s: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut lit = String::new();
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' if matches!(chars.peek(), Some((_, '[' | ']' | '{' | '}' | '\\'))) => {
                let (_, next) = chars.next().expect("peeked");
                lit.push(next);
            }
            '[' => {
                let Some(end) = s[i + 1..].find(']') else {
                    lit.push(c);
                    continue;
                };
                let inner = &s[i + 1..i + 1 + end];
                match range(inner)? {
                    Some(values) => {
                        parts.push(Part::Lit(std::mem::take(&mut lit)));
                        parts.push(Part::Choices(values));
                        // Skip past the closing bracket.
                        while let Some((j, _)) = chars.peek() {
                            if *j > i + 1 + end {
                                break;
                            }
                            chars.next();
                        }
                    }
                    None => lit.push(c),
                }
            }
            '{' => {
                let Some(end) = s[i + 1..].find('}') else {
                    return Err("A { list isn't closed with }.".into());
                };
                let inner = &s[i + 1..i + 1 + end];
                let words: Vec<String> = inner.split(',').map(str::to_string).collect();
                if words.len() < 2 {
                    lit.push(c);
                    continue;
                }
                parts.push(Part::Lit(std::mem::take(&mut lit)));
                parts.push(Part::Choices(words));
                while let Some((j, _)) = chars.peek() {
                    if *j > i + 1 + end {
                        break;
                    }
                    chars.next();
                }
            }
            _ => lit.push(c),
        }
    }
    parts.push(Part::Lit(lit));
    Ok(parts)
}

/// The values of `a-b` or `a-b:step`; None when it isn't a range at all.
fn range(inner: &str) -> Result<Option<Vec<String>>, String> {
    let (span, step) = match inner.split_once(':') {
        Some((span, step)) if step.chars().all(|c| c.is_ascii_digit()) && !step.is_empty() => (
            span,
            step.parse::<u64>().map_err(|_| "The step is too large.")?,
        ),
        Some(_) => return Ok(None),
        None => (inner, 1),
    };
    let Some((a, b)) = span.split_once('-') else {
        return Ok(None);
    };
    if step == 0 {
        return Err("The step of a range can't be 0.".into());
    }
    let step_us = usize::try_from(step).unwrap_or(usize::MAX);
    let digits = |x: &str| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit());
    if digits(a) && digits(b) {
        let (Ok(from), Ok(to)) = (a.parse::<u64>(), b.parse::<u64>()) else {
            return Err("A number in the range is too large.".into());
        };
        if from > to {
            return Err(format!(
                "The range [{a}-{b}] counts down; write it as [{b}-{a}]."
            ));
        }
        if (to - from) / step >= MAX_URLS as u64 {
            return Err(format!(
                "The range [{span}] gives more than {MAX_URLS} addresses."
            ));
        }
        let width = if a.len() > 1 && a.starts_with('0') {
            a.len()
        } else {
            0
        };
        return Ok(Some(
            (from..=to)
                .step_by(step_us)
                .map(|n| format!("{n:0width$}"))
                .collect(),
        ));
    }
    let letter = |x: &str| {
        let mut it = x.chars();
        match (it.next(), it.next()) {
            (Some(c), None) if c.is_ascii_alphabetic() => Some(c),
            _ => None,
        }
    };
    if let (Some(from), Some(to)) = (letter(a), letter(b)) {
        if from.is_ascii_lowercase() != to.is_ascii_lowercase() {
            return Err(format!(
                "The range [{a}-{b}] mixes small and capital letters."
            ));
        }
        if from > to {
            return Err(format!(
                "The range [{a}-{b}] counts down; write it as [{b}-{a}]."
            ));
        }
        return Ok(Some(
            (from..=to)
                .step_by(step_us)
                .map(|c| c.to_string())
                .collect(),
        ));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_letters_steps_and_lists() {
        assert_eq!(
            expand("https://x/img[1-3].jpg").unwrap(),
            [
                "https://x/img1.jpg",
                "https://x/img2.jpg",
                "https://x/img3.jpg"
            ]
        );
        let padded = expand("https://x/[008-011].png").unwrap();
        assert_eq!(
            padded,
            [
                "https://x/008.png",
                "https://x/009.png",
                "https://x/010.png",
                "https://x/011.png"
            ]
        );
        assert_eq!(
            expand("https://x/[a-c]").unwrap(),
            ["https://x/a", "https://x/b", "https://x/c"]
        );
        assert_eq!(
            expand("https://x/[A-B]").unwrap(),
            ["https://x/A", "https://x/B"]
        );
        assert_eq!(
            expand("https://x/p[0-10:5]").unwrap(),
            ["https://x/p0", "https://x/p5", "https://x/p10"]
        );
        assert_eq!(
            expand("https://x/f.{jpg,png}").unwrap(),
            ["https://x/f.jpg", "https://x/f.png"]
        );
        let product = expand("https://x/[1-2]_{a,b}").unwrap();
        assert_eq!(
            product,
            [
                "https://x/1_a",
                "https://x/1_b",
                "https://x/2_a",
                "https://x/2_b"
            ]
        );
    }

    #[test]
    fn keeps_what_isnt_a_pattern() {
        assert_eq!(
            expand("http://[::1]:8080/f[1-2]").unwrap(),
            ["http://[::1]:8080/f1", "http://[::1]:8080/f2"]
        );
        assert_eq!(
            expand("https://x/\\[1-2\\]/[1-2]").unwrap(),
            ["https://x/[1-2]/1", "https://x/[1-2]/2"]
        );
        assert_eq!(
            expand("https://x/{one}/[1-2]").unwrap(),
            ["https://x/{one}/1", "https://x/{one}/2"]
        );
    }

    #[test]
    fn refuses_bad_or_huge_patterns() {
        assert!(expand("https://x/plain").is_err());
        assert!(expand("https://x/[9-1]").is_err());
        assert!(expand("https://x/[a-Z]").is_err());
        assert!(expand("https://x/[1-5:0]").is_err());
        assert!(expand("https://x/{a,b").is_err());
        assert!(expand("https://x/[1-100000]").is_err());
        assert!(
            expand("https://x/[1-200]/[1-200]")
                .unwrap_err()
                .contains("40000")
        );
        assert_eq!(expand("https://x/[1-10000]").unwrap().len(), 10_000);
    }
}
