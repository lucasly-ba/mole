//! Driving the `tesseract` binary as a subprocess.
//!
//! Rather than link Tesseract's C API, we shell out to the `tesseract` command:
//! the captured frame is piped in as a PPM and word boxes come back as TSV. This
//! keeps the build free of an extra native dependency (the binary is only needed
//! at runtime) and makes the OCR engine trivial to swap later (PaddleOCR, say).
//!
//! Pipeline: screenshot → PPM on stdin → `tesseract` → TSV on stdout.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use super::cancel::Cancel;
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
    ///
    /// `cancel` lets another thread kill this read in flight (so an on-demand
    /// hint can preempt the background pre-warm): the child is shared behind a
    /// lock that [`Cancel::abort`] can reach, while its output is read off the
    /// lock so the killer is never blocked. An aborted run returns an error.
    pub fn run(&self, ppm: Vec<u8>, threads: usize, cancel: &Cancel) -> Result<String> {
        if cancel.aborted() {
            return Err(Error::Ocr("OCR cancelled".into()));
        }
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

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Ocr("no stdin handle".into()))?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Ocr("no stdout handle".into()))?;

        // Write on a separate thread so a large image can't deadlock against a
        // full stdout pipe. Dropping stdin closes it, signalling EOF to tesseract.
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&ppm);
        });

        // Share the child so an [`Cancel::abort`] on another thread can kill it
        // while we read its output here. Reading happens off the lock, so a
        // killer that grabs the lock is never blocked waiting on us.
        let child = Arc::new(Mutex::new(child));
        let token = cancel.register(Arc::clone(&child));

        let mut out = Vec::new();
        let read = stdout.read_to_end(&mut out);
        let status = child.lock().unwrap().wait();
        cancel.unregister(token);
        let _ = writer.join();

        // A killed child surfaces as a read EOF plus a signal exit; report it as a
        // cancellation rather than a spurious OCR failure.
        if cancel.aborted() {
            return Err(Error::Ocr("OCR cancelled".into()));
        }
        read.map_err(|e| Error::Ocr(format!("reading tesseract output: {e}")))?;
        let status = status.map_err(|e| Error::Ocr(format!("tesseract failed: {e}")))?;
        if !status.success() {
            return Err(Error::Ocr(format!("tesseract exited with {status}")));
        }
        String::from_utf8(out)
            .map_err(|e| Error::Ocr(format!("tesseract produced non-UTF8 TSV: {e}")))
    }
}

/// Encode the rectangle `[x0, x0 + w) × [y0, y0 + h)` of a tight RGB buffer as a
/// PPM — one band of the screen for tiled OCR. `stride_w` is the full buffer width
/// in pixels; the band is usually narrower (it stays within one monitor), so this
/// copies the cropped columns row by row. `rgb` is the whole-screen buffer from
/// [`Screen::to_rgb`], reused across bands so it is built only once. Rows that
/// fall outside the buffer are zero-padded so the emitted image always matches the
/// declared `w × h` (a malformed PPM would make Tesseract reject the whole band).
pub fn encode_ppm_band(rgb: &[u8], stride_w: i32, x0: i32, y0: i32, w: i32, h: i32) -> Vec<u8> {
    let stride = stride_w.max(0) as usize * 3;
    let row_bytes = w.max(0) as usize * 3;
    let xoff = x0.max(0) as usize * 3;
    let mut out = Vec::with_capacity(16 + row_bytes * h.max(0) as usize);
    out.extend_from_slice(format!("P6\n{w} {h}\n255\n").as_bytes());
    for y in 0..h.max(0) {
        let base = ((y0 + y).max(0) as usize * stride + xoff).min(rgb.len());
        let avail = rgb.len().saturating_sub(base).min(row_bytes);
        out.extend_from_slice(&rgb[base..base + avail]);
        out.resize(out.len() + (row_bytes - avail), 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pixel `i`'s RGB, distinct per pixel: `(3i, 3i+1, 3i+2)`.
    fn px(i: u8) -> [u8; 3] {
        [i * 3, i * 3 + 1, i * 3 + 2]
    }

    #[test]
    fn encode_crops_a_subrectangle_of_the_buffer() {
        // A 4×2 image, pixels 0..8 left-to-right, top-to-bottom.
        let rgb: Vec<u8> = (0..8u8).flat_map(px).collect();
        // Crop columns [1, 3) over both rows -> a 2×2 band.
        let ppm = encode_ppm_band(&rgb, 4, 1, 0, 2, 2);
        let header = b"P6\n2 2\n255\n";
        assert!(ppm.starts_with(header), "PPM header declares the band size");
        let body = &ppm[header.len()..];
        // Row 0: pixels 1,2 ; row 1: pixels 5,6 — the cropped columns only.
        let expect: Vec<u8> = [1u8, 2, 5, 6].iter().flat_map(|&i| px(i)).collect();
        assert_eq!(body, &expect[..]);
    }

    #[test]
    fn encode_zero_pads_rows_that_fall_off_the_buffer() {
        // Ask for a band taller than the 4×2 buffer: the missing third row must be
        // padded so the emitted image still matches the declared 4×3 size.
        let rgb: Vec<u8> = (0..8u8).flat_map(px).collect();
        let ppm = encode_ppm_band(&rgb, 4, 0, 0, 4, 3);
        let header = b"P6\n4 3\n255\n";
        let body = &ppm[header.len()..];
        assert_eq!(body.len(), 4 * 3 * 3, "exactly width*height*3 bytes");
        assert_eq!(&body[..24], &rgb[..24], "real rows copied verbatim");
        assert!(
            body[24..].iter().all(|&b| b == 0),
            "missing row zero-padded"
        );
    }
}
