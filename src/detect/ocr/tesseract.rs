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

use crate::error::{Error, Result};

/// A configured handle to the OCR binary. Cheap to clone/rebuild on config
/// reload; holds no process or connection between runs.
#[derive(Clone)]
pub struct Tesseract {
    binary: String,
    language: String,
}

impl Tesseract {
    pub fn new(binary: String, language: String) -> Self {
        Tesseract { binary, language }
    }

    /// Run the OCR binary, feeding `ppm` on stdin and returning the TSV stdout.
    ///
    /// `threads` caps Tesseract's internal OpenMP threads via `OMP_THREAD_LIMIT`
    /// (`0` leaves it to Tesseract). When several instances run in parallel
    /// (tiled OCR), capping each one keeps them from oversubscribing the cores
    /// and fighting each other.
    pub fn run(&self, ppm: Vec<u8>, threads: usize) -> Result<String> {
        // `--psm 11` is "sparse text": find as much text as possible anywhere on
        // the image, which is what we want for a whole, busy desktop.
        let mut cmd = Command::new(&self.binary);
        cmd.args(["-", "-", "-l", &self.language, "--psm", "11", "tsv"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if threads > 0 {
            cmd.env("OMP_THREAD_LIMIT", threads.to_string());
        }
        let mut child = cmd
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

/// Encode rows `[y0, y0 + height)` of a tight, full-width RGB buffer as a PPM —
/// one horizontal strip of the screen for tiled OCR. `rgb` is the whole-screen
/// buffer from [`Screen::to_rgb`], reused across strips so it is built only once.
pub fn encode_ppm_band(rgb: &[u8], width: i32, y0: i32, height: i32) -> Vec<u8> {
    let w = width.max(0) as usize;
    let start = (y0.max(0) as usize * w * 3).min(rgb.len());
    let end = ((y0 + height).max(0) as usize * w * 3).min(rgb.len());
    let body = &rgb[start..end];
    let mut out = Vec::with_capacity(16 + body.len());
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    out.extend_from_slice(body);
    out
}
