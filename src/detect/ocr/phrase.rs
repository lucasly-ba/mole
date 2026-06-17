//! Grouping OCR words into phrase targets.
//!
//! Tesseract gives us one box per *word*. Hinting every single word would bury
//! the screen in labels and make "jump to that menu entry" a multi-hint chore.
//! Instead we merge words that read as a unit (a run of words on the same line,
//! separated by ordinary spacing) into one [`Element`] whose box spans the
//! whole phrase and whose text is the words joined back together.
//!
//! The grouping is purely geometric (it never re-reads pixels), so it is fast,
//! deterministic and exhaustively unit-tested below; no display required.

use crate::detect::Element;
use crate::geometry::Rect;

/// A single recognised word: its on-screen box and text. Confidence filtering
/// happens upstream in [`super::tsv`], so every `Word` here is one we trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub rect: Rect,
    pub text: String,
}

impl Word {
    pub fn new(rect: Rect, text: impl Into<String>) -> Self {
        Word {
            rect,
            text: text.into(),
        }
    }
}

/// Tunables controlling how aggressively words coalesce into phrases. Mirrors
/// the `[ocr]` config fields of the same name.
#[derive(Debug, Clone, Copy)]
pub struct Grouping {
    /// Two words share a line when their vertical centres differ by at most this
    /// fraction of their text height.
    pub line_tolerance: f64,
    /// Consecutive words on a line join the same phrase when the gap between
    /// them is at most this multiple of their text height.
    pub max_word_gap: f64,
}

/// Merge `words` into phrase elements: cluster into lines top-to-bottom, then
/// split each line into phrases wherever a wide horizontal gap (a column break,
/// a separate UI control) interrupts the flow.
pub fn group(mut words: Vec<Word>, g: Grouping) -> Vec<Element> {
    words.retain(|w| !w.text.trim().is_empty() && w.rect.width > 0 && w.rect.height > 0);
    if words.is_empty() {
        return Vec::new();
    }

    // Reading order first: top-to-bottom, then left-to-right.
    words.sort_by_key(|w| (w.rect.center().y, w.rect.center().x));

    // 1. Partition the sorted words into lines.
    let mut lines: Vec<Vec<Word>> = Vec::new();
    for w in words {
        match lines.last_mut() {
            Some(line) if same_line(line, &w, g.line_tolerance) => line.push(w),
            _ => lines.push(vec![w]),
        }
    }

    // 2. Split each line into phrases on wide gaps, emit one Element per phrase.
    let mut out = Vec::new();
    for mut line in lines {
        line.sort_by_key(|w| w.rect.x);
        let mut phrase: Vec<Word> = Vec::new();
        for w in line {
            if let Some(prev) = phrase.last() {
                let gap = w.rect.x - prev.rect.right();
                let limit = (g.max_word_gap * w.rect.height as f64) as i32;
                if gap > limit {
                    out.push(finish(&phrase));
                    phrase = Vec::new();
                }
            }
            phrase.push(w);
        }
        if !phrase.is_empty() {
            out.push(finish(&phrase));
        }
    }
    out
}

/// Whether `w` belongs to the line built so far, judged against the line's mean
/// vertical centre so a slightly skewed row doesn't drift apart.
fn same_line(line: &[Word], w: &Word, tolerance: f64) -> bool {
    let mean_y = line.iter().map(|p| p.rect.center().y).sum::<i32>() / line.len() as i32;
    let height = line
        .iter()
        .map(|p| p.rect.height)
        .min()
        .unwrap_or(w.rect.height)
        .min(w.rect.height)
        .max(1);
    (w.rect.center().y - mean_y).abs() <= (tolerance * height as f64) as i32
}

/// Collapse a phrase's words into one [`Element`]: the union of their boxes, and
/// their texts joined with single spaces.
fn finish(words: &[Word]) -> Element {
    let rect = words
        .iter()
        .map(|w| w.rect)
        .reduce(|a, b| a.union(&b))
        .expect("phrase is never empty");
    let text = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Element::new(rect, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word `height` tall on a given baseline, `width` px wide at `x`.
    fn word(x: i32, y: i32, width: i32, height: i32, text: &str) -> Word {
        Word::new(Rect::new(x, y, width, height), text)
    }

    /// Generous defaults: words within ~one text-height gap join.
    fn grouping() -> Grouping {
        Grouping {
            line_tolerance: 0.5,
            max_word_gap: 1.0,
        }
    }

    #[test]
    fn adjacent_words_on_a_line_become_one_phrase() {
        // "Hello" then "World" with a normal space between them.
        let words = vec![
            word(10, 100, 50, 16, "Hello"),
            word(66, 100, 50, 16, "World"),
        ];
        let out = group(words, grouping());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "Hello World");
        // Box spans from the first word's left to the second word's right.
        assert_eq!(out[0].rect, Rect::new(10, 100, 106, 16));
    }

    #[test]
    fn a_wide_gap_splits_into_separate_phrases() {
        // Two columns far apart on the same line: a menu bar entry and a clock.
        let words = vec![word(10, 20, 40, 16, "File"), word(900, 20, 60, 16, "12:00")];
        let out = group(words, grouping());
        assert_eq!(out.len(), 2, "the column gap breaks the phrase");
        assert_eq!(out[0].text, "File");
        assert_eq!(out[1].text, "12:00");
    }

    #[test]
    fn different_lines_never_merge() {
        let words = vec![
            word(10, 100, 50, 16, "top"),
            word(10, 200, 50, 16, "bottom"),
        ];
        let out = group(words, grouping());
        assert_eq!(out.len(), 2);
        // Reading order: the higher line comes first.
        assert_eq!(out[0].text, "top");
        assert_eq!(out[1].text, "bottom");
    }

    #[test]
    fn slightly_skewed_words_still_share_a_line() {
        // Baselines off by 2px (sub-pixel OCR jitter) on 16px text.
        let words = vec![
            word(10, 100, 40, 16, "a"),
            word(54, 102, 40, 16, "b"),
            word(98, 99, 40, 16, "c"),
        ];
        let out = group(words, grouping());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "a b c");
    }

    #[test]
    fn max_word_gap_zero_keeps_words_separate() {
        // With no tolerance for gaps, even a normal space splits words.
        let g = Grouping {
            line_tolerance: 0.5,
            max_word_gap: 0.0,
        };
        let words = vec![
            word(10, 100, 50, 16, "Hello"),
            word(66, 100, 50, 16, "World"),
        ];
        let out = group(words, g);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn blank_and_degenerate_words_are_dropped() {
        let words = vec![
            word(10, 100, 50, 16, "   "), // whitespace only
            word(70, 100, 0, 16, "zero"), // zero width
            word(80, 100, 50, 16, "real"),
        ];
        let out = group(words, grouping());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "real");
    }

    #[test]
    fn no_words_yields_no_phrases() {
        assert!(group(vec![], grouping()).is_empty());
    }
}
