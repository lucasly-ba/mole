//! OCR fallback detection (Plan §3.3).
//!
//! For apps that expose nothing to AT-SPI — Electron, terminals, games — we fall
//! back to optical character recognition. Rather than link Tesseract's C API, we
//! shell out to the `tesseract` binary: the captured frame is piped in as a PPM
//! and word boxes come back as TSV. This keeps the build free of extra native
//! dependencies (the binary is only needed at runtime) and is easy to swap for
//! PaddleOCR later (Plan: "PaddleOCR envisagé").
//!
//! Pipeline: screenshot → PPM on stdin → `tesseract` → TSV → word rectangles.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::capture::Screen;
use crate::config::Config;
use crate::detect::{self, Detector, Element};
use crate::error::{Error, Result};
use crate::geometry::Rect;

/// Minimum Tesseract confidence (0..100) for a word to become a hint.
const MIN_CONFIDENCE: f32 = 50.0;

/// Detector that runs Tesseract over a screen capture.
pub struct OcrDetector {
    binary: String,
    language: String,
    min_element_size: i32,
}

impl OcrDetector {
    pub fn new(config: &Config) -> Self {
        OcrDetector {
            binary: config.ocr.binary.clone(),
            language: config.ocr.language.clone(),
            min_element_size: config.ocr.min_element_size,
        }
    }

    /// Encode a screen capture as a binary PPM (P6) for Tesseract/leptonica.
    fn encode_ppm(screen: &Screen) -> Vec<u8> {
        let w = screen.width();
        let h = screen.height();
        let mut out = Vec::with_capacity(16 + (w * h * 3) as usize);
        out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
        for y in 0..h {
            for x in 0..w {
                let c = screen
                    .pixel(crate::geometry::Point::new(screen.bounds().x + x, screen.bounds().y + y))
                    .0;
                out.push(c[0]); // R
                out.push(c[1]); // G
                out.push(c[2]); // B
            }
        }
        out
    }

    /// Run the OCR binary, feeding `ppm` on stdin and returning the TSV stdout.
    fn run_tesseract(&self, ppm: Vec<u8>) -> Result<String> {
        let mut child = Command::new(&self.binary)
            .args(["-", "-", "-l", &self.language, "--psm", "11", "tsv"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Error::Ocr(format!("failed to spawn {}: {e}", self.binary)))?;

        // Write on a separate thread so a large image can't deadlock against a
        // full stdout pipe.
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Ocr("no stdin handle".into()))?;
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&ppm);
            // Dropping stdin here closes it, signalling EOF to tesseract.
        });

        let output = child
            .wait_with_output()
            .map_err(|e| Error::Ocr(format!("tesseract failed: {e}")))?;
        let _ = writer.join();

        if !output.status.success() {
            return Err(Error::Ocr(format!(
                "tesseract exited with {}",
                output.status
            )));
        }
        String::from_utf8(output.stdout)
            .map_err(|e| Error::Ocr(format!("tesseract produced non-UTF8 TSV: {e}")))
    }
}

impl Detector for OcrDetector {
    fn name(&self) -> &'static str {
        "ocr"
    }

    fn detect(&self, screen: &Screen) -> Result<Vec<Element>> {
        let ppm = Self::encode_ppm(screen);
        let tsv = self.run_tesseract(ppm)?;
        let words = parse_tsv(&tsv, screen.bounds());
        Ok(detect::finalize(words, self.min_element_size, screen.bounds()))
    }
}

/// Parse Tesseract TSV output into word-level elements, offset into absolute
/// screen coordinates.
fn parse_tsv(tsv: &str, region: Rect) -> Vec<Element> {
    let mut elements = Vec::new();
    let mut lines = tsv.lines();

    // Header maps column names to indices, so we don't hard-code the layout.
    let Some(header) = lines.next() else {
        return elements;
    };
    let cols: Vec<&str> = header.split('\t').collect();
    let idx = |name: &str| cols.iter().position(|c| *c == name);

    let (Some(i_level), Some(i_left), Some(i_top), Some(i_w), Some(i_h), Some(i_conf), Some(i_text)) = (
        idx("level"),
        idx("left"),
        idx("top"),
        idx("width"),
        idx("height"),
        idx("conf"),
        idx("text"),
    ) else {
        log::warn!("unexpected tesseract TSV header: {header:?}");
        return elements;
    };

    for line in lines {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() <= i_text {
            continue;
        }
        // Level 5 is a word.
        if f[i_level] != "5" {
            continue;
        }
        let conf: f32 = f[i_conf].parse().unwrap_or(-1.0);
        let text = f[i_text].trim();
        if conf < MIN_CONFIDENCE || text.is_empty() {
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
        elements.push(Element::new(rect, text));
    }
    elements
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_word_rows_and_offsets_to_screen() {
        let tsv = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n\
                   1\t1\t0\t0\t0\t0\t0\t0\t1920\t1080\t-1\t\n\
                   5\t1\t1\t1\t1\t1\t10\t20\t40\t12\t96\tFile\n\
                   5\t1\t1\t1\t1\t2\t60\t20\t40\t12\t30\tlowconf\n\
                   5\t1\t1\t1\t1\t3\t120\t20\t10\t12\t99\t   ";
        let els = parse_tsv(tsv, Rect::new(100, 200, 1920, 1080));
        assert_eq!(els.len(), 1, "only the high-confidence non-blank word");
        assert_eq!(els[0].text, "File");
        // Offset by the region origin.
        assert_eq!(els[0].rect, Rect::new(110, 220, 40, 12));
    }

    #[test]
    fn empty_tsv_is_no_elements() {
        assert!(parse_tsv("", Rect::new(0, 0, 100, 100)).is_empty());
    }
}
