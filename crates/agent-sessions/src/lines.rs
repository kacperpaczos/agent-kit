//! Bounded JSONL reading: streaming lines with a size cap, the first few
//! lines of a file, and the last complete lines of a file.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// One raw line from [`LineReader`].
pub enum Line<'a> {
    Text(&'a [u8]),
    /// The line exceeded the cap; its bytes were skipped.
    TooLong,
}

/// Reads `\n`-separated lines, never buffering more than `max` bytes.
pub struct LineReader<R> {
    inner: R,
    max: usize,
    buf: Vec<u8>,
}

impl<R: BufRead> LineReader<R> {
    pub fn new(inner: R, max: usize) -> Self {
        Self {
            inner,
            max,
            buf: Vec::new(),
        }
    }

    /// The next line without its terminator; `None` at EOF.
    pub fn next_line(&mut self) -> io::Result<Option<Line<'_>>> {
        self.buf.clear();
        let mut overflow = false;
        let mut any = false;
        loop {
            let available = match self.inner.fill_buf() {
                Ok(bytes) => bytes,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if available.is_empty() {
                break;
            }
            any = true;
            let (chunk, used, done) = match available.iter().position(|&b| b == b'\n') {
                Some(pos) => (&available[..pos], pos + 1, true),
                None => (available, available.len(), false),
            };
            if !overflow {
                if self.buf.len() + chunk.len() > self.max {
                    overflow = true;
                    self.buf.clear();
                } else {
                    self.buf.extend_from_slice(chunk);
                }
            }
            self.inner.consume(used);
            if done {
                break;
            }
        }
        if !any {
            return Ok(None);
        }
        if overflow {
            return Ok(Some(Line::TooLong));
        }
        if self.buf.last() == Some(&b'\r') {
            self.buf.pop();
        }
        Ok(Some(Line::Text(&self.buf)))
    }
}

/// Open `path` for bounded line reading.
pub fn open(path: &Path, max: usize) -> io::Result<LineReader<BufReader<File>>> {
    Ok(LineReader::new(BufReader::new(File::open(path)?), max))
}

/// Up to `max_lines` parsed JSON objects from the start of `path`, plus
/// the number of lines that were not valid JSON.
pub fn head(path: &Path, max_lines: usize, max_line: usize) -> io::Result<(Vec<Value>, usize)> {
    let mut reader = open(path, max_line)?;
    let mut values = Vec::new();
    let mut broken = 0;
    let mut seen = 0;
    while seen < max_lines {
        let Some(line) = reader.next_line()? else {
            break;
        };
        seen += 1;
        match line {
            Line::Text(bytes) if bytes.iter().all(u8::is_ascii_whitespace) => {}
            Line::Text(bytes) => match serde_json::from_slice::<Value>(bytes) {
                Ok(v) => values.push(v),
                Err(_) => broken += 1,
            },
            Line::TooLong => {}
        }
    }
    Ok((values, broken))
}

/// Parsed JSON objects from the last complete lines of `path`, last line
/// first. Reads at most `window` bytes from the end.
pub fn tail(path: &Path, window: u64) -> io::Result<Vec<Value>> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(window);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(window).read_to_end(&mut bytes)?;
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    if start > 0 && !lines.is_empty() {
        // The first piece is (probably) the tail of a cut line.
        lines.remove(0);
    }
    Ok(lines
        .into_iter()
        .rev()
        .filter_map(|l| serde_json::from_slice::<Value>(l).ok())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(input: &str, max: usize) -> Vec<Option<String>> {
        let mut r = LineReader::new(io::Cursor::new(input.as_bytes().to_vec()), max);
        let mut out = Vec::new();
        while let Some(line) = r.next_line().unwrap() {
            out.push(match line {
                Line::Text(b) => Some(String::from_utf8(b.to_vec()).unwrap()),
                Line::TooLong => None,
            });
        }
        out
    }

    #[test]
    fn splits_and_caps_lines() {
        assert_eq!(
            collect("ab\r\ncdefgh\nij", 4),
            vec![Some("ab".into()), None, Some("ij".into())]
        );
        assert_eq!(collect("", 4), Vec::<Option<String>>::new());
        assert_eq!(collect("\n", 4), vec![Some(String::new())]);
    }
}
