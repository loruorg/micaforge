//! The JSON reader: an array of records, or one record per line.
//!
//! PostHog exports events as JSON as often as it exports them as CSV: a batch
//! export to S3 writes one event per line, a download from the events API is
//! an array, and the API import in [`crate::import_api`] writes one event per
//! line to its scratch file. Simple Analytics' export API answers one data
//! point per line too. This reader turns all of those into the same staged
//! records the CSV reader produces, through the same [`Shaper`], so the
//! normaliser never learns which reader a record came through.
//!
//! # Streamed, never parsed whole
//!
//! An export is up to two gigabytes, and `serde_json::from_reader` into a
//! `Vec` would hold all of it, several times over once it is a tree of
//! `Value`s. Both shapes are therefore read one record at a time: a stream of
//! values for the line-per-record shape, and a sequence visitor that hands each
//! element on as soon as it is parsed for the array shape. Memory is one
//! record, whatever the size of the file.
//!
//! # The shape is read from the bytes, not the file name
//!
//! An upload and an API download are both staged at the same scratch path,
//! whatever the original name was, so the first byte decides: `[` is an
//! array, `{` is one object per line (or simply one object after another,
//! which is the same stream to the parser), and anything else is CSV. A CSV
//! export starts with a header row, and no header row starts with a brace.

use std::cell::Cell;
use std::io::{BufRead, Read};

use micaforge_core::{Error, Result};
use serde::de::{Error as _, SeqAccess, Visitor};
use serde::Deserializer as _;
use serde_json::Value as Json;

use crate::import_record::Shaper;
use crate::run_import::Stager;

/// The byte-order mark some Windows tools put in front of UTF-8 text.
const BOM: &[u8] = b"\xEF\xBB\xBF";

/// What a source file turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Comma-separated, with a header row.
    Csv,
    /// One JSON array of records.
    Array,
    /// JSON records one after another, usually one per line.
    Lines,
    /// Nothing at all.
    Empty,
}

/// Decide what a file is from its first non-blank byte.
///
/// Looks without consuming, so a CSV reader handed the same reader afterwards
/// still sees the file from its first byte. A byte-order mark in front of JSON
/// is consumed, because the JSON parser would refuse it.
pub fn sniff<R: BufRead>(reader: &mut R) -> Result<Shape> {
    let buffer = reader
        .fill_buf()
        .map_err(|e| Error::BadRequest(format!("the export could not be read: {e}")))?;
    if buffer.is_empty() {
        return Ok(Shape::Empty);
    }

    let body = buffer.strip_prefix(BOM).unwrap_or(buffer);
    let shape = match body.iter().find(|b| !b.is_ascii_whitespace()) {
        Some(b'[') => Shape::Array,
        Some(b'{') => Shape::Lines,
        _ => Shape::Csv,
    };
    if shape != Shape::Csv && buffer.starts_with(BOM) {
        reader.consume(BOM.len());
    }
    Ok(shape)
}

/// Stage every record of a JSON file of the given shape.
pub(crate) fn read<R: BufRead>(
    reader: R,
    shape: Shape,
    shaper: &Shaper,
    stager: &mut Stager,
) -> Result<()> {
    let position = Cell::new(0u64);
    let counted = Counted {
        inner: reader,
        seen: &position,
    };

    match shape {
        Shape::Array => {
            let mut parser = serde_json::Deserializer::from_reader(counted);
            let mut failure = None;
            let parsed = (&mut parser).deserialize_seq(Elements {
                shaper,
                stager: &mut *stager,
                position: &position,
                failure: &mut failure,
            });
            if let Some(e) = failure {
                return Err(e);
            }
            parsed.map_err(unreadable)?;
            parser.end().map_err(unreadable)
        }
        Shape::Lines => {
            let stream = serde_json::Deserializer::from_reader(counted).into_iter::<Json>();
            for value in stream {
                take(shaper, stager, value.map_err(unreadable)?, position.get())?;
            }
            Ok(())
        }
        Shape::Csv | Shape::Empty => Ok(()),
    }
}

/// Stage one parsed value, if it is a record this run still needs.
fn take(shaper: &Shaper, stager: &mut Stager, value: Json, byte_pos: u64) -> Result<()> {
    if !stager.admit() {
        return Ok(());
    }
    let Json::Object(object) = value else {
        // Something that is not an object in a list of events is not an
        // event. It is counted with the records that had no time.
        stager.skip();
        return Ok(());
    };
    let mut record = shaper.record();
    for (name, value) in object {
        record.column(&name, value);
    }
    stager.push(record.finish(), byte_pos)
}

/// A parse failure, as the import row will show it.
fn unreadable(e: serde_json::Error) -> Error {
    Error::BadRequest(format!("the export stopped being readable: {e}"))
}

/// Hands each element of a top-level array on as soon as it is parsed.
struct Elements<'a> {
    shaper: &'a Shaper,
    stager: &'a mut Stager,
    position: &'a Cell<u64>,
    /// Why staging stopped, when it was the writer and not the parser. Kept
    /// aside because the visitor can only return a parser error.
    failure: &'a mut Option<Error>,
}

impl<'de> Visitor<'de> for Elements<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an array of event objects")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<(), A::Error> {
        while let Some(value) = seq.next_element::<Json>()? {
            if let Err(e) = take(self.shaper, self.stager, value, self.position.get()) {
                *self.failure = Some(e);
                return Err(A::Error::custom("staging stopped"));
            }
        }
        Ok(())
    }
}

/// A reader that counts what it hands over, for the progress bar.
struct Counted<'a, R> {
    inner: R,
    seen: &'a Cell<u64>,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.seen.set(self.seen.get() + n as u64);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape_of(bytes: &[u8]) -> Shape {
        sniff(&mut std::io::BufReader::new(bytes)).unwrap()
    }

    #[test]
    fn the_first_byte_decides_the_shape() {
        assert_eq!(shape_of(b"[{\"event\":\"x\"}]"), Shape::Array);
        assert_eq!(
            shape_of(b"\n  {\"event\":\"x\"}\n{\"event\":\"y\"}"),
            Shape::Lines
        );
        assert_eq!(shape_of(b"uuid,event,timestamp\n"), Shape::Csv);
        assert_eq!(shape_of(b"# GA4 preamble\ndate,views\n"), Shape::Csv);
        assert_eq!(shape_of(b""), Shape::Empty);
    }

    #[test]
    fn a_byte_order_mark_is_consumed_in_front_of_json_only() {
        let mut json = std::io::BufReader::new(&b"\xEF\xBB\xBF[1]"[..]);
        assert_eq!(sniff(&mut json).unwrap(), Shape::Array);
        assert_eq!(json.fill_buf().unwrap(), b"[1]");

        // The CSV reader strips its own, and gets the file untouched.
        let mut csv = std::io::BufReader::new(&b"\xEF\xBB\xBFdate,views"[..]);
        assert_eq!(sniff(&mut csv).unwrap(), Shape::Csv);
        assert!(csv.fill_buf().unwrap().starts_with(BOM));
    }
}
