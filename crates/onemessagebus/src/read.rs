//! Reading streams back, resuming from a position, and merging several.
//!
//! A [`Reader`] reads one stream file from a byte position at a record
//! boundary and yields each envelope with the position after it — a position a
//! later reader resumes from. A torn final line is a *reading*, never an error
//! that ends the read: the whole records before it are yielded, the torn bytes
//! are reported with the position they start at, and the resume position is
//! the one after the last whole record — so a writer that completes the line
//! is read whole on the next resume. [`Merge`] folds several readers in
//! `(ts, stream, seq)` order.

use std::fs::File;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::envelope::Envelope;
use crate::vocabulary::Vocabulary;

/// One whole record, and where the next one starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Record<V: Vocabulary> {
    /// The envelope the line held.
    pub envelope: Envelope<V>,
    /// The byte position after this record — what a later reader resumes from
    /// to read only what follows it.
    pub position: u64,
}

/// A final line no newline has ended yet: a writer is still on it, or died on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Torn {
    /// The byte position the torn bytes start at, which is also the position
    /// to resume from: the one after the last whole record.
    pub at: u64,
    /// How many bytes of it there are so far.
    pub bytes: u64,
}

/// A whole line that is not an envelope.
///
/// Reported rather than skipped: a line this build cannot read is the line an
/// operator most needs to see, and a reader that dropped it would answer
/// "nothing there" for "could not read".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The byte position the line starts at.
    pub at: u64,
    /// The byte position after it, where the next record starts.
    pub position: u64,
    /// Why it is not an envelope, as serde said it.
    pub reason: String,
}

/// What one line of a stream read as.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading<V: Vocabulary> {
    /// A whole envelope.
    Record(Record<V>),
    /// The unfinished final line.
    Torn(Torn),
    /// A whole line that is not an envelope.
    Refused(Refused),
}

/// A reader over one stream file, from a byte position at a record boundary.
#[derive(Debug)]
pub struct Reader<V: Vocabulary> {
    path: PathBuf,
    readings: std::vec::IntoIter<Reading<V>>,
    position: u64,
}

impl<V: Vocabulary> Reader<V> {
    /// Open the stream at `path` from its start.
    ///
    /// # Errors
    ///
    /// The file could not be read.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        Self::open_at(path, 0)
    }

    /// Open the stream at `path` from `position`, a byte position at a record
    /// boundary — the one a [`Record`] or a [`Torn`] reading handed back.
    ///
    /// # Errors
    ///
    /// The file could not be read, or `position` is not a record boundary —
    /// past the end of the file, or not just after a newline — refused as
    /// [`InvalidInput`](std::io::ErrorKind::InvalidInput) naming the position.
    /// A position inside a record would read its tail as a record of its own.
    pub fn open_at(path: impl AsRef<Path>, position: u64) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        if position > 0 {
            // The one-byte read that checks the boundary also leaves the
            // cursor at `position`, so the read below needs no second seek.
            file.seek(SeekFrom::Start(position - 1))?;
            let mut before = [0u8; 1];
            let why = match file.read(&mut before)? {
                0 => Some("it is past the end of the file"),
                _ if before[0] != b'\n' => {
                    Some("it is inside a record: no newline ends the byte before it")
                }
                _ => None,
            };
            if let Some(why) = why {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "cannot read {} from byte {position}: {why}; resume from a position a reading handed back, or 0",
                        path.display()
                    ),
                ));
            }
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let readings = read_lines::<V>(&bytes, position);
        let resume = readings
            .iter()
            .rev()
            .find_map(|reading| match reading {
                Reading::Record(record) => Some(record.position),
                Reading::Refused(refused) => Some(refused.position),
                Reading::Torn(_) => None,
            })
            .unwrap_or(position);
        Ok(Self {
            path,
            readings: readings.into_iter(),
            position: resume,
        })
    }

    /// The file this reader reads.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The position after the last whole record: what a later reader resumes
    /// from to yield only what was appended after this read.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Every whole envelope, in the order the file holds them, with the torn
    /// tail and any refused line reported beside them.
    pub fn collect_all(self) -> Readings<V> {
        let mut readings = Readings {
            position: self.position,
            records: Vec::new(),
            refused: Vec::new(),
            torn: None,
        };
        for reading in self {
            match reading {
                Reading::Record(record) => readings.records.push(record),
                Reading::Refused(refused) => readings.refused.push(refused),
                Reading::Torn(torn) => readings.torn = Some(torn),
            }
        }
        readings
    }
}

impl<V: Vocabulary> Iterator for Reader<V> {
    type Item = Reading<V>;

    fn next(&mut self) -> Option<Self::Item> {
        self.readings.next()
    }
}

/// Everything one read of a stream produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Readings<V: Vocabulary> {
    /// The position after the last whole record.
    pub position: u64,
    /// The whole envelopes, in file order.
    pub records: Vec<Record<V>>,
    /// The whole lines that were not envelopes.
    pub refused: Vec<Refused>,
    /// The unfinished final line, if there is one.
    pub torn: Option<Torn>,
}

/// The lines of `bytes`, which start at `offset` in the file, each read as far
/// as it can be.
fn read_lines<V: Vocabulary>(bytes: &[u8], offset: u64) -> Vec<Reading<V>> {
    let mut readings = Vec::new();
    let mut start = 0usize;
    while start < bytes.len() {
        let Some(newline) = bytes[start..].iter().position(|byte| *byte == b'\n') else {
            readings.push(Reading::Torn(Torn {
                at: offset + start as u64,
                bytes: (bytes.len() - start) as u64,
            }));
            break;
        };
        let end = start + newline;
        let line = &bytes[start..end];
        let position = offset + end as u64 + 1;
        readings.push(match parse_line::<V>(line) {
            Ok(envelope) => Reading::Record(Record { envelope, position }),
            Err(reason) => Reading::Refused(Refused {
                at: offset + start as u64,
                position,
                reason,
            }),
        });
        start = end + 1;
    }
    readings
}

/// One whole line as an envelope, or why it is not one.
fn parse_line<V: Vocabulary>(line: &[u8]) -> Result<Envelope<V>, String> {
    let text = std::str::from_utf8(line).map_err(|failure| format!("not UTF-8: {failure}"))?;
    if text.trim().is_empty() {
        return Err("an empty line, which no writer leaves".to_owned());
    }
    serde_json::from_str(text).map_err(|failure| failure.to_string())
}

/// Several streams folded into one, in `(ts, stream, seq)` order.
#[derive(Debug)]
pub struct Merge<V: Vocabulary> {
    records: Vec<Envelope<V>>,
    refused: Vec<(PathBuf, Refused)>,
    torn: Vec<(PathBuf, Torn)>,
}

impl<V: Vocabulary> Merge<V> {
    /// Fold every reading of `readers` into one ordered stream.
    #[must_use]
    pub fn new(readers: impl IntoIterator<Item = Reader<V>>) -> Self {
        let mut records = Vec::new();
        let mut refused = Vec::new();
        let mut torn = Vec::new();
        for reader in readers {
            let path = reader.path().to_path_buf();
            for reading in reader {
                match reading {
                    Reading::Record(record) => records.push(record.envelope),
                    Reading::Refused(line) => refused.push((path.clone(), line)),
                    Reading::Torn(tail) => torn.push((path.clone(), tail)),
                }
            }
        }
        // A stable sort, so two envelopes with one key — which the contract
        // does not promise apart — stay in the order they were read.
        records.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        Self {
            records,
            refused,
            torn,
        }
    }

    /// Open every path and fold the streams.
    ///
    /// # Errors
    ///
    /// The first path that could not be read, with the path named.
    pub fn open(paths: impl IntoIterator<Item = impl AsRef<Path>>) -> std::io::Result<Self> {
        let mut readers = Vec::new();
        for path in paths {
            let path = path.as_ref();
            readers.push(Reader::open(path).map_err(|failure| {
                std::io::Error::new(
                    failure.kind(),
                    format!("cannot read {}: {failure}", path.display()),
                )
            })?);
        }
        Ok(Self::new(readers))
    }

    /// The merged envelopes, in `(ts, stream, seq)` order.
    #[must_use]
    pub fn records(&self) -> &[Envelope<V>] {
        &self.records
    }

    /// The whole lines that were not envelopes, with the file each was in.
    #[must_use]
    pub fn refused(&self) -> &[(PathBuf, Refused)] {
        &self.refused
    }

    /// The torn tails, with the file each was in.
    #[must_use]
    pub fn torn(&self) -> &[(PathBuf, Torn)] {
        &self.torn
    }

    /// The merged envelopes, taken.
    #[must_use]
    pub fn into_records(self) -> Vec<Envelope<V>> {
        self.records
    }
}
