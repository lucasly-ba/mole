//! Driving the `tesseract` binary as a subprocess.
//!
//! Rather than link Tesseract's C API, we shell out to the `tesseract` command:
//! the captured frame is piped in as a PPM and word boxes come back as TSV. This
//! keeps the build free of an extra native dependency (the binary is only needed
//! at runtime) and makes the OCR engine trivial to swap later (PaddleOCR, say).
//!
//! Pipeline: screenshot → PPM on stdin → `tesseract` → TSV on stdout.

use std::io::Write;
use std::process::{Command, Stdio};

use crate::capture::Screen;
use crate::error::{Error, Result};

/// A configured handle to the OCR binary. Cheap to clone/rebuild on config
/// reload; holds no process or connection between runs.
pub struct Tesseract {
    binary: String,
    language: String,
}

impl Tesseract {
    pub fn new(binary: String, language: String) -> Self {
        Tesseract { binary, language }
    }

    /// Read `screen` and return Tesseract's raw TSV output.
    pub fn read(&self, screen: &Screen) -> Result<String> {
        let ppm = encode_ppm(screen);
        self.run(ppm)
    }

    /// Run the OCR binary, feeding `ppm` on stdin and returning the TSV stdout.
    fn run(&self, ppm: Vec<u8>) -> Result<String> {
        // `--psm 11` is "sparse text": find as much text as possible anywhere on
        // the image, which is what we want for a whole, busy desktop.
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

/// Encode a screen capture as a binary PPM (P6) for Tesseract/leptonica: a tiny
/// header followed by the tight RGB bytes.
fn encode_ppm(screen: &Screen) -> Vec<u8> {
    let (w, h) = (screen.width(), screen.height());
    let body = screen.to_rgb();
    let mut out = Vec::with_capacity(16 + body.len());
    out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    out.extend_from_slice(&body);
    out
}
