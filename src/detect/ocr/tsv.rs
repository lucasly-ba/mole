//! Parsing Tesseract's TSV output into word boxes.
//!
//! Tesseract, run with the `tsv` config, prints one row per layout node with a
//! `level` column (1=page … 5=word) and pixel geometry. We keep the level-5
//! word rows that clear a confidence threshold and turn them into [`Word`]s in
//! absolute screen coordinates; grouping into phrases is [`super::phrase`]'s job.

use crate::geometry::Rect;

use super::phrase::Word;

/// The Tesseract layout level for an individual word.
const WORD_LEVEL: &str = "5";

/// Parse `tsv` into confident word boxes, offset by the captured `region`'s
/// origin so the boxes are in absolute screen coordinates.
pub fn parse(tsv: &str, region: Rect, min_confidence: f32) -> Vec<Word> {
    let mut words = Vec::new();
    let mut lines = tsv.lines();

    // The header maps column names to indices, so we never hard-code the layout.
    let Some(header) = lines.next() else {
        return words;
    };
    let cols: Vec<&str> = header.split('\t').collect();
    let idx = |name: &str| cols.iter().position(|c| *c == name);

    let (
        Some(i_level),
        Some(i_left),
        Some(i_top),
        Some(i_w),
        Some(i_h),
        Some(i_conf),
        Some(i_text),
    ) = (
        idx("level"),
        idx("left"),
        idx("top"),
        idx("width"),
        idx("height"),
        idx("conf"),
        idx("text"),
    )
    else {
        log::warn!("unexpected tesseract TSV header: {header:?}");
        return words;
    };

    for line in lines {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() <= i_text || f[i_level] != WORD_LEVEL {
            continue;
        }
        let conf: f32 = f[i_conf].parse().unwrap_or(-1.0);
        let text = f[i_text].trim();
        if conf < min_confidence || text.is_empty() {
            continue;
        }
        let (Ok(left), Ok(top), Ok(w), Ok(h)) = (
            f[i_left].parse::<i32>(),
            f[i_top].parse::<i32>(),
            f[i_w].parse::<i32>(),
            f[i_h].parse::<i32>(),
        ) else {
            continue;
        };
        let rect = Rect::new(region.x + left, region.y + top, w, h);
        words.push(Word::new(rect, text));
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str =
        "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";

    #[test]
    fn keeps_confident_words_and_offsets_to_screen() {
        let tsv = format!(
            "{HEADER}\n\
             1\t1\t0\t0\t0\t0\t0\t0\t1920\t1080\t-1\t\n\
             5\t1\t1\t1\t1\t1\t10\t20\t40\t12\t96\tFile\n\
             5\t1\t1\t1\t1\t2\t60\t20\t40\t12\t30\tlowconf\n\
             5\t1\t1\t1\t1\t3\t120\t20\t10\t12\t99\t   "
        );
        let words = parse(&tsv, Rect::new(100, 200, 1920, 1080), 50.0);
        assert_eq!(words.len(), 1, "only the confident, non-blank word");
        assert_eq!(words[0].text, "File");
        assert_eq!(words[0].rect, Rect::new(110, 220, 40, 12));
    }

    #[test]
    fn confidence_threshold_is_honoured() {
        let tsv = format!("{HEADER}\n5\t1\t1\t1\t1\t1\t10\t20\t40\t12\t40\tmeh");
        assert!(parse(&tsv, Rect::new(0, 0, 100, 100), 50.0).is_empty());
        assert_eq!(parse(&tsv, Rect::new(0, 0, 100, 100), 30.0).len(), 1);
    }

    #[test]
    fn empty_tsv_is_no_words() {
        assert!(parse("", Rect::new(0, 0, 100, 100), 50.0).is_empty());
    }
}
