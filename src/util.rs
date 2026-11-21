use unicode_width::UnicodeWidthChar;

pub fn char_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

pub fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{11FF}'   // Hangul Jamo
        | '\u{2E80}'..='\u{2EFF}' // CJK radicals
        | '\u{3000}'..='\u{303F}' // CJK punctuation
        | '\u{3040}'..='\u{30FF}' // Hiragana/Katakana
        | '\u{3400}'..='\u{4DBF}' // CJK ext A
        | '\u{4E00}'..='\u{9FFF}' // CJK unified
        | '\u{AC00}'..='\u{D7AF}' // Hangul syllables
        | '\u{F900}'..='\u{FAFF}' // CJK compatibility
        | '\u{FF00}'..='\u{FFEF}' // fullwidth forms
    )
}

pub fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ').count()
}

pub fn escape(s: &str) -> String {
    html_escape::encode_text(s).into_owned()
}

/// Wrap one code line at `max` display columns. Continuations get two spaces.
/// Tabs are expanded to four spaces first; indentation is otherwise preserved.
pub fn wrap_code_line(line: &str, max: usize) -> Vec<String> {
    let expanded = line.replace('\t', "    ");
    if display_width(&expanded) <= max {
        return vec![expanded];
    }

    let mut out = Vec::new();
    let mut rest = expanded.as_str();
    let mut limit = max;

    while display_width(rest) > limit {
        let mut width = 0usize;
        let mut hard_cut = rest.len();
        let mut soft_cut: Option<usize> = None;

        for (idx, ch) in rest.char_indices() {
            let cw = char_width(ch);
            if width + cw > limit {
                hard_cut = idx;
                break;
            }
            width += cw;
            if matches!(
                ch,
                ' ' | '-' | '_' | '/' | '.' | ',' | ';' | ':' | '(' | '=' | '+'
            ) {
                soft_cut = Some(idx + ch.len_utf8());
            }
        }

        let break_at = soft_cut
            .filter(|b| *b > limit / 3)
            .unwrap_or(hard_cut)
            .max(1);
        let (head, tail) = rest.split_at(break_at);
        let head = head.trim_end();
        if out.is_empty() {
            out.push(head.to_string());
        } else {
            out.push(format!("  {}", head.trim_start()));
        }
        rest = tail.trim_start();
        limit = max.saturating_sub(2);
    }

    if !rest.is_empty() {
        if out.is_empty() {
            out.push(rest.to_string());
        } else {
            out.push(format!("  {rest}"));
        }
    }
    out
}

/// Soft-wrap prose for TXT. `width == 0` leaves paragraphs unwrapped.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_w = 0usize;

    for word in text.split_whitespace() {
        let ww = display_width(word);
        if current.is_empty() {
            if ww <= width {
                current.push_str(word);
                current_w = ww;
            } else {
                // A single long token (URL, CJK run): hard-split it.
                let mut chunk = String::new();
                let mut chunk_w = 0;
                for ch in word.chars() {
                    let cw = char_width(ch);
                    if chunk_w + cw > width && !chunk.is_empty() {
                        lines.push(chunk);
                        chunk = String::new();
                        chunk_w = 0;
                    }
                    chunk.push(ch);
                    chunk_w += cw;
                }
                current = chunk;
                current_w = chunk_w;
            }
        } else if current_w + 1 + ww <= width {
            current.push(' ');
            current.push_str(word);
            current_w += 1 + ww;
        } else {
            lines.push(current);
            current = word.to_string();
            current_w = ww;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub fn underline_for(text: &str) -> String {
    let n = display_width(text).clamp(3, 72);
    "=".repeat(n)
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_wrap_cuts_at_the_column_and_indents_continuations() {
        let line = "x".repeat(60);
        let wrapped = wrap_code_line(&line, 45);
        assert_eq!(wrapped.len(), 2);
        assert_eq!(display_width(&wrapped[0]), 45);
        assert!(wrapped[1].starts_with("  "));
        assert_eq!(wrapped[0].len() + (wrapped[1].len() - 2), 60);
    }

    #[test]
    fn code_wrap_prefers_a_break_character() {
        let wrapped = wrap_code_line("let value = compute(alpha, beta, gamma);", 24);
        assert!(wrapped.len() > 1);
        assert!(wrapped.iter().skip(1).all(|line| line.starts_with("  ")));
    }
}
