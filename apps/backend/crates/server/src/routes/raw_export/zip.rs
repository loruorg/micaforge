//! A ZIP writer that never holds an archive, only the next few bytes of one.
//!
//! `routes::raw_export` streams an archive whose size is not known until the
//! last row has been read, so this writer has to put each file's header on the
//! wire before that file's data exists. Three choices follow from that, and
//! each one is the format's own answer to the problem rather than a workaround:
//!
//! - **General-purpose bit 3 on every entry.** The local header carries a CRC
//!   and two sizes of zero, and the real values follow the data in a *data
//!   descriptor*. Every reader honours it, because it is how `zip -` writes to
//!   a pipe. The central directory, written last, repeats the real values, so a
//!   reader that starts from the end (which is nearly all of them) never sees
//!   the zeros at all.
//! - **Deflate only.** A deflate stream marks its own end, so a reader walking
//!   the local headers from the front can find where an entry stops without the
//!   sizes. A stored entry has no such marker, which is why a streamed archive
//!   must not use one.
//! - **ZIP32, not ZIP64.** ZIP64 is a second set of records that a streamed
//!   writer has to commit to in the local header, before it knows whether the
//!   entry will need them, and the row cap in `routes::raw_export` keeps a real
//!   export far below 4 GiB anyway. So the writer stays ZIP32, and
//!   [`ZipWriter::room_for`] is the guard that makes the limit a stopping point
//!   rather than a corrupt file: it answers `false` before any size or offset
//!   could pass [`ZIP32_MAX`], and the caller ends that file there and says so.
//!
//! The writer does no I/O. Bytes accumulate in an output buffer and the caller
//! drains it with [`ZipWriter::take`] after each step, which is what lets the
//! HTTP body pull the archive at the client's pace.
//!
//! Every [`ZipWriter::write`] ends with a deflate *sync flush*. The flush costs
//! a few bytes per call and ends a deflate block early, which is negligible at
//! the chunk sizes the caller uses, and it buys the property the size guard is
//! built on: after each write, every input byte is in the output, so the
//! archive's length so far is exact rather than an estimate of what the
//! compressor might still be holding.

use std::io;

use chrono::{DateTime, Datelike, Timelike, Utc};
use flate2::{Compress, Compression, Crc, FlushCompress, Status};

/// Signature of a local file header.
const LOCAL_HEADER: u32 = 0x0403_4b50;

/// Signature of a data descriptor. Optional in the specification and written
/// anyway: some readers look for it, and none is confused by it.
const DATA_DESCRIPTOR: u32 = 0x0807_4b50;

/// Signature of a central directory file header.
const CENTRAL_HEADER: u32 = 0x0201_4b50;

/// Signature of the end of central directory record.
const END_OF_DIRECTORY: u32 = 0x0605_4b50;

/// Version 2.0, the first that defines deflate and the data descriptor.
const VERSION_NEEDED: u16 = 20;

/// Made by Unix (host 3, high byte) to version 2.0 (low byte), so the external
/// attributes below are read as Unix file permissions.
const VERSION_MADE_BY: u16 = (3 << 8) | VERSION_NEEDED;

/// General-purpose bit 3: the CRC and sizes follow the data.
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;

/// Compression method 8, deflate.
const METHOD_DEFLATE: u16 = 8;

/// A regular file, readable by all and writable by its owner, in the high half
/// of the external attributes where Unix permissions go.
const EXTERNAL_ATTRIBUTES: u32 = 0o100_644 << 16;

/// The largest size or offset a ZIP32 field may hold.
///
/// One below `u32::MAX`, because `0xFFFF_FFFF` in a size or offset field is the
/// marker that tells a reader to look for a ZIP64 record instead, and this
/// writer never writes one.
pub const ZIP32_MAX: u64 = 0xFFFF_FFFE;

/// Room kept back for everything that follows the last data file: that file's
/// final deflate block and data descriptor, the README, the central directory
/// and the end record.
///
/// The README is a few kilobytes at most and the directory holds four entries
/// of under a hundred bytes each, so 64 KiB is a wide margin rather than a
/// tight one. A test in `routes::raw_export` holds the README to it.
pub const TAIL_RESERVE: u64 = 64 * 1024;

/// Spare output capacity made available before each call into the compressor.
const OUT_STEP: usize = 64 * 1024;

/// One finished entry, as the central directory will describe it.
#[derive(Debug, Clone)]
struct Entry {
    /// The file name. A constant from the caller, never caller text.
    name: &'static str,
    /// Where the entry's local header starts.
    offset: u64,
    /// CRC-32 of the uncompressed data.
    crc: u32,
    /// Size of the deflate stream.
    packed: u64,
    /// Size of the data before compression.
    raw: u64,
}

/// The entry being written.
struct OpenEntry {
    /// The file name.
    name: &'static str,
    /// Where its local header starts.
    offset: u64,
    /// Running CRC-32 of everything written to it.
    crc: Crc,
    /// A raw deflate stream, with no zlib header: ZIP frames the data itself.
    deflate: Compress,
}

/// A ZIP archive, produced a step at a time.
pub struct ZipWriter {
    /// Bytes produced and not yet taken by the caller.
    out: Vec<u8>,
    /// Bytes the caller has already taken.
    taken: u64,
    /// The modification time stamped on every entry, in MS-DOS form.
    dos_time: u16,
    /// The modification date stamped on every entry, in MS-DOS form.
    dos_date: u16,
    /// Entries already finished, in the order they were written.
    entries: Vec<Entry>,
    /// The entry being written, if any.
    open: Option<OpenEntry>,
    /// Set once the central directory has been written.
    closed: bool,
}

impl ZipWriter {
    /// An empty archive whose entries are all stamped `modified`.
    ///
    /// MS-DOS time has no zone, so the stamp is the UTC wall clock, which is
    /// also the zone every timestamp inside the archive is written in.
    pub fn new(modified: DateTime<Utc>) -> ZipWriter {
        let (dos_time, dos_date) = dos_timestamp(modified);
        ZipWriter {
            out: Vec::with_capacity(OUT_STEP),
            taken: 0,
            dos_time,
            dos_date,
            entries: Vec::new(),
            open: None,
            closed: false,
        }
    }

    /// How many bytes of archive exist so far, taken or not. This is also the
    /// offset the next byte will have.
    pub fn len(&self) -> u64 {
        self.taken + self.out.len() as u64
    }

    /// Hand over every byte produced since the last call.
    pub fn take(&mut self) -> Vec<u8> {
        let bytes = std::mem::replace(&mut self.out, Vec::with_capacity(OUT_STEP));
        self.taken += bytes.len() as u64;
        bytes
    }

    /// Begin a new entry, finishing the one still open first, so two entries
    /// can never interleave.
    pub fn start(&mut self, name: &'static str) -> io::Result<()> {
        if self.closed {
            return Err(refused("the archive is already closed"));
        }
        self.finish_entry()?;

        let offset = self.len();
        if offset > ZIP32_MAX {
            return Err(refused("the archive is past the ZIP32 limit"));
        }
        let name_len = name_length(name)?;

        self.put_u32(LOCAL_HEADER);
        self.put_u16(VERSION_NEEDED);
        self.put_u16(FLAG_DATA_DESCRIPTOR);
        self.put_u16(METHOD_DEFLATE);
        self.put_u16(self.dos_time);
        self.put_u16(self.dos_date);
        // CRC, compressed size, uncompressed size: all in the data descriptor.
        self.put_u32(0);
        self.put_u32(0);
        self.put_u32(0);
        self.put_u16(name_len);
        self.put_u16(0);
        self.out.extend_from_slice(name.as_bytes());

        self.open = Some(OpenEntry {
            name,
            offset,
            crc: Crc::new(),
            deflate: Compress::new(Compression::default(), false),
        });
        Ok(())
    }

    /// Could `more` uncompressed bytes go into the open entry and still leave
    /// [`TAIL_RESERVE`] for the rest of the archive inside ZIP32?
    ///
    /// This is the question a data file asks before each row. `false` when no
    /// entry is open.
    pub fn room_for(&self, more: usize) -> bool {
        self.fits(more, TAIL_RESERVE)
    }

    /// Two limits, checked together: the open entry's own uncompressed size,
    /// and the end of the archive once `more` bytes in their worst-case
    /// compressed form and `reserve` are added. The second is exact up to
    /// `more`, because every [`ZipWriter::write`] flushes, so
    /// [`ZipWriter::len`] already counts every byte written before.
    fn fits(&self, more: usize, reserve: u64) -> bool {
        let Some(open) = &self.open else {
            return false;
        };
        let more = more as u64;
        let raw_after = open.deflate.total_in().saturating_add(more);
        let end_after = self
            .len()
            .saturating_add(deflate_ceiling(more))
            .saturating_add(reserve);
        raw_after <= ZIP32_MAX && end_after <= ZIP32_MAX
    }

    /// Compress `data` into the open entry, then flush, so the bytes this call
    /// produced are a complete and decodable continuation of the entry.
    ///
    /// A data file asks [`ZipWriter::room_for`] first. This checks the hard
    /// limit without the reserve, which is what lets the README, written last,
    /// spend the reserve kept for it, and still turns a caller that forgot to
    /// ask into an error rather than an archive whose sizes have wrapped.
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if !self.fits(data.len(), 0) {
            return Err(refused(
                "no entry is open, or the data would pass the ZIP32 limit",
            ));
        }
        if let Some(open) = self.open.as_mut() {
            open.crc.update(data);
        }
        self.deflate(data, FlushCompress::Sync)
    }

    /// End the open entry: its last deflate block, then its data descriptor.
    /// Does nothing when no entry is open.
    pub fn finish_entry(&mut self) -> io::Result<()> {
        if self.open.is_none() {
            return Ok(());
        }
        self.deflate(&[], FlushCompress::Finish)?;
        let Some(open) = self.open.take() else {
            return Ok(());
        };

        let entry = Entry {
            name: open.name,
            offset: open.offset,
            crc: open.crc.sum(),
            packed: open.deflate.total_out(),
            raw: open.deflate.total_in(),
        };
        let packed = zip32(entry.packed, "compressed size")?;
        let raw = zip32(entry.raw, "uncompressed size")?;

        self.put_u32(DATA_DESCRIPTOR);
        self.put_u32(entry.crc);
        self.put_u32(packed);
        self.put_u32(raw);

        self.entries.push(entry);
        Ok(())
    }

    /// Finish any open entry, then write the central directory and the end
    /// record. Nothing can be added afterwards.
    pub fn finish(&mut self) -> io::Result<()> {
        if self.closed {
            return Ok(());
        }
        self.finish_entry()?;

        let directory_start = self.len();
        let count = u16::try_from(self.entries.len())
            .map_err(|_| refused("more entries than ZIP32 can list"))?;

        let entries = std::mem::take(&mut self.entries);
        for entry in &entries {
            self.put_u32(CENTRAL_HEADER);
            self.put_u16(VERSION_MADE_BY);
            self.put_u16(VERSION_NEEDED);
            self.put_u16(FLAG_DATA_DESCRIPTOR);
            self.put_u16(METHOD_DEFLATE);
            self.put_u16(self.dos_time);
            self.put_u16(self.dos_date);
            self.put_u32(entry.crc);
            self.put_u32(zip32(entry.packed, "compressed size")?);
            self.put_u32(zip32(entry.raw, "uncompressed size")?);
            self.put_u16(name_length(entry.name)?);
            // Extra field, comment, disk number, internal attributes.
            self.put_u16(0);
            self.put_u16(0);
            self.put_u16(0);
            self.put_u16(0);
            self.put_u32(EXTERNAL_ATTRIBUTES);
            self.put_u32(zip32(entry.offset, "entry offset")?);
            self.out.extend_from_slice(entry.name.as_bytes());
        }
        self.entries = entries;

        let directory_size = self.len() - directory_start;
        self.put_u32(END_OF_DIRECTORY);
        // This disk, and the disk the directory starts on: there is one.
        self.put_u16(0);
        self.put_u16(0);
        self.put_u16(count);
        self.put_u16(count);
        self.put_u32(zip32(directory_size, "directory size")?);
        self.put_u32(zip32(directory_start, "directory offset")?);
        // No archive comment.
        self.put_u16(0);

        self.closed = true;
        Ok(())
    }

    /// Run the open entry's compressor over `input` with `flush`, growing the
    /// output buffer as it fills.
    ///
    /// `compress_vec` only writes into spare capacity, so capacity is made
    /// before every call. The loop ends when the input is used up and the
    /// compressor stopped short of filling the space it was given, which is
    /// how it says it has nothing more to emit for this flush; for
    /// [`FlushCompress::Finish`] it ends when the stream reports its end.
    fn deflate(&mut self, mut input: &[u8], flush: FlushCompress) -> io::Result<()> {
        let Some(open) = self.open.as_mut() else {
            return Err(refused("no entry is open"));
        };
        let finishing = flush == FlushCompress::Finish;

        loop {
            self.out.reserve(OUT_STEP);
            let before = open.deflate.total_in();
            let status = open
                .deflate
                .compress_vec(input, &mut self.out, flush)
                .map_err(io::Error::other)?;
            let used = usize::try_from(open.deflate.total_in() - before)
                .map_err(|_| refused("the compressor reported an impossible length"))?;
            input = input.get(used..).unwrap_or_default();

            let room_left = self.out.len() < self.out.capacity();
            match status {
                Status::StreamEnd => return Ok(()),
                _ if finishing => {}
                _ if input.is_empty() && room_left => return Ok(()),
                _ => {}
            }
        }
    }

    fn put_u16(&mut self, value: u16) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u32(&mut self, value: u32) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }
}

/// The most deflate can turn `n` bytes into, with a wide margin.
///
/// Where compression would expand the data, deflate falls back to stored
/// blocks, which cost five bytes per 64 KiB; zlib's own `deflateBound` is about
/// `n + n/4096 + 13`. An eighth on top is far above anything the encoder emits,
/// and it costs little, because [`ZipWriter::room_for`] only ever applies it to
/// the one chunk about to be written, never to the whole entry.
fn deflate_ceiling(n: u64) -> u64 {
    n.saturating_add(n / 8).saturating_add(64)
}

/// MS-DOS time and date for `t`: two-second resolution, years 1980 to 2107.
fn dos_timestamp(t: DateTime<Utc>) -> (u16, u16) {
    let year = t.year().clamp(1980, 2107) - 1980;
    // Every component is range-limited by `chrono` (hour < 24, minute < 60,
    // second < 61, month 1..=12, day 1..=31) and the year is clamped above,
    // so each shifted field fits its bits and the casts cannot truncate.
    let time = ((t.hour() as u16) << 11) | ((t.minute() as u16) << 5) | ((t.second() / 2) as u16);
    let date = ((year as u16) << 9) | ((t.month() as u16) << 5) | (t.day() as u16);
    (time, date)
}

/// A size or offset as a ZIP32 field, or an error past [`ZIP32_MAX`].
fn zip32(value: u64, what: &str) -> io::Result<u32> {
    if value > ZIP32_MAX {
        return Err(refused(&format!("the {what} is past the ZIP32 limit")));
    }
    u32::try_from(value).map_err(|_| refused(&format!("the {what} does not fit")))
}

/// A file name's length as its 16-bit field.
fn name_length(name: &str) -> io::Result<u16> {
    u16::try_from(name.len()).map_err(|_| refused("the file name is too long"))
}

/// The writer's own refusal, as an I/O error.
fn refused(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, format!("zip writer: {why}"))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use chrono::TimeZone;
    use flate2::{Decompress, FlushDecompress};

    fn le16(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn le32(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    }

    /// Read an archive back the way a strict reader would, checking every field
    /// this writer is responsible for on the way, and return its files.
    ///
    /// Written here rather than taken from a ZIP crate because the point is to
    /// check the bytes against the specification, and a library would accept
    /// several things this writer must not produce.
    pub(in crate::routes::raw_export) fn read_archive(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        // The end record, with no comment, is the last 22 bytes.
        let end = bytes.len() - 22;
        assert_eq!(le32(bytes, end), END_OF_DIRECTORY, "end record");
        assert_eq!(le16(bytes, end + 4), 0, "one disk");
        let count = usize::from(le16(bytes, end + 8));
        assert_eq!(le16(bytes, end + 10) as usize, count);
        let directory_size = le32(bytes, end + 12) as usize;
        let directory_start = le32(bytes, end + 16) as usize;
        assert_eq!(le16(bytes, end + 20), 0, "no comment");
        assert_eq!(
            directory_start + directory_size,
            end,
            "the directory ends where the end record starts"
        );

        let mut files = Vec::new();
        let mut at = directory_start;
        let mut expected_offset = 0usize;
        for _ in 0..count {
            assert_eq!(le32(bytes, at), CENTRAL_HEADER);
            assert_eq!(le16(bytes, at + 4), VERSION_MADE_BY);
            assert_eq!(le16(bytes, at + 6), VERSION_NEEDED);
            assert_eq!(le16(bytes, at + 8), FLAG_DATA_DESCRIPTOR);
            assert_eq!(le16(bytes, at + 10), METHOD_DEFLATE);
            let crc = le32(bytes, at + 16);
            let packed = le32(bytes, at + 20) as usize;
            let raw = le32(bytes, at + 24) as usize;
            let name_len = usize::from(le16(bytes, at + 28));
            assert_eq!(le16(bytes, at + 30), 0, "no extra field");
            assert_eq!(le16(bytes, at + 32), 0, "no comment");
            assert_eq!(le32(bytes, at + 38), EXTERNAL_ATTRIBUTES);
            let offset = le32(bytes, at + 42) as usize;
            let name = String::from_utf8(bytes[at + 46..at + 46 + name_len].to_vec()).unwrap();
            at += 46 + name_len;

            // Entries are laid out back to back, in directory order.
            assert_eq!(offset, expected_offset, "{name}: local header offset");

            // The local header: sizes and CRC deferred to the descriptor.
            assert_eq!(le32(bytes, offset), LOCAL_HEADER, "{name}");
            assert_eq!(le16(bytes, offset + 4), VERSION_NEEDED);
            assert_eq!(le16(bytes, offset + 6), FLAG_DATA_DESCRIPTOR);
            assert_eq!(le16(bytes, offset + 8), METHOD_DEFLATE);
            assert_eq!(le32(bytes, offset + 14), 0, "{name}: CRC deferred");
            assert_eq!(le32(bytes, offset + 18), 0, "{name}: size deferred");
            assert_eq!(le32(bytes, offset + 22), 0, "{name}: size deferred");
            assert_eq!(usize::from(le16(bytes, offset + 26)), name_len);
            assert_eq!(le16(bytes, offset + 28), 0, "no extra field");
            assert_eq!(&bytes[offset + 30..offset + 30 + name_len], name.as_bytes());

            // The data: a raw deflate stream that ends exactly at `packed`.
            let data_start = offset + 30 + name_len;
            let mut inflate = Decompress::new(false);
            let mut data = Vec::with_capacity(raw + 1);
            let status = inflate
                .decompress_vec(
                    &bytes[data_start..data_start + packed],
                    &mut data,
                    FlushDecompress::Finish,
                )
                .unwrap();
            assert_eq!(status, Status::StreamEnd, "{name}: the stream ends");
            assert_eq!(inflate.total_in() as usize, packed, "{name}: packed size");
            assert_eq!(data.len(), raw, "{name}: raw size");
            let mut check = Crc::new();
            check.update(&data);
            assert_eq!(check.sum(), crc, "{name}: CRC");

            // The data descriptor repeats what the directory says.
            let descriptor = data_start + packed;
            assert_eq!(le32(bytes, descriptor), DATA_DESCRIPTOR, "{name}");
            assert_eq!(le32(bytes, descriptor + 4), crc);
            assert_eq!(le32(bytes, descriptor + 8) as usize, packed);
            assert_eq!(le32(bytes, descriptor + 12) as usize, raw);
            expected_offset = descriptor + 16;

            files.push((name, data));
        }
        assert_eq!(expected_offset, directory_start, "nothing between files");
        assert_eq!(at, end, "the directory holds exactly `count` entries");
        files
    }

    fn stamp() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 8, 13, 45, 31).unwrap()
    }

    /// Bytes that do not compress: a linear congruential sequence.
    fn noise(len: usize) -> Vec<u8> {
        let mut state: u32 = 0x1234_5678;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn an_archive_reads_back_file_for_file() {
        let mut zip = ZipWriter::new(stamp());
        let mut wire = Vec::new();

        zip.start("events.csv").unwrap();
        zip.write(b"a,b\n1,2\n").unwrap();
        zip.write(b"3,4\n").unwrap();
        wire.extend(zip.take());
        zip.start("README.txt").unwrap();
        zip.write("caf\u{e9} \u{1f600}\n".as_bytes()).unwrap();
        zip.finish().unwrap();
        wire.extend(zip.take());

        assert_eq!(zip.len(), wire.len() as u64, "len counts taken bytes too");
        let files = read_archive(&wire);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "events.csv");
        assert_eq!(files[0].1, b"a,b\n1,2\n3,4\n");
        assert_eq!(files[1].0, "README.txt");
        assert_eq!(files[1].1, "caf\u{e9} \u{1f600}\n".as_bytes());
    }

    #[test]
    fn an_empty_file_is_still_a_valid_entry() {
        let mut zip = ZipWriter::new(stamp());
        zip.start("errors.ndjson").unwrap();
        zip.finish().unwrap();
        let files = read_archive(&zip.take());
        assert_eq!(files, vec![("errors.ndjson".to_string(), Vec::new())]);
    }

    #[test]
    fn data_that_will_not_compress_and_data_that_will_both_survive() {
        let random = noise(300 * 1024);
        let repeated = b"2026-10-08T13:45:31.000Z,/docs/,Firefox\n".repeat(20_000);

        let mut zip = ZipWriter::new(stamp());
        let mut wire = Vec::new();
        zip.start("random.bin").unwrap();
        for piece in random.chunks(70_000) {
            zip.write(piece).unwrap();
            wire.extend(zip.take());
        }
        zip.start("repeated.csv").unwrap();
        for piece in repeated.chunks(128 * 1024) {
            zip.write(piece).unwrap();
            wire.extend(zip.take());
        }
        zip.finish().unwrap();
        wire.extend(zip.take());

        let files = read_archive(&wire);
        assert_eq!(files[0].1, random);
        assert_eq!(files[1].1, repeated);
        // Repetitive text must actually compress, flushes and all.
        assert!(wire.len() < random.len() + repeated.len() / 10);
    }

    #[test]
    fn every_write_is_flushed_so_the_length_is_exact() {
        let mut zip = ZipWriter::new(stamp());
        zip.start("events.csv").unwrap();
        let after_header = zip.len();
        zip.write(&b"x".repeat(10_000)).unwrap();
        let after_write = zip.len();
        assert!(after_write > after_header, "the flush emitted the data");

        // A sync flush ends on a byte boundary with an empty stored block:
        // 00 00 FF FF is the tail of that marker.
        let bytes = zip.take();
        assert_eq!(&bytes[bytes.len() - 4..], &[0x00, 0x00, 0xFF, 0xFF]);
    }

    #[test]
    fn local_headers_defer_the_crc_and_sizes() {
        let mut zip = ZipWriter::new(stamp());
        zip.start("web_vitals.csv").unwrap();
        let header = zip.take();
        assert_eq!(header.len(), 30 + "web_vitals.csv".len());
        assert_eq!(le32(&header, 0), LOCAL_HEADER);
        assert_eq!(le16(&header, 6), 1 << 3, "bit 3: data descriptor");
        assert_eq!(le16(&header, 8), 8, "deflate");
        assert_eq!(&header[14..26], &[0u8; 12], "CRC and both sizes are zero");
        assert_eq!(&header[30..], b"web_vitals.csv");
    }

    #[test]
    fn the_timestamp_is_ms_dos_wall_clock_in_utc() {
        let (time, date) = dos_timestamp(stamp());
        assert_eq!(time >> 11, 13);
        assert_eq!((time >> 5) & 0x3F, 45);
        assert_eq!(time & 0x1F, 15, "two-second resolution");
        assert_eq!(date >> 9, 2026 - 1980);
        assert_eq!((date >> 5) & 0x0F, 10);
        assert_eq!(date & 0x1F, 8);

        // Out of the format's range in either direction: clamped, not wrapped.
        let early = Utc.with_ymd_and_hms(1970, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(dos_timestamp(early).1 >> 9, 0);
        let late = Utc.with_ymd_and_hms(2200, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(dos_timestamp(late).1 >> 9, 127);
    }

    #[test]
    fn the_size_guard_stops_short_of_the_zip32_limit() {
        let mut zip = ZipWriter::new(stamp());
        assert!(!zip.room_for(1), "no entry is open");
        assert!(zip.write(b"x").is_err(), "and nothing can be written");

        zip.start("events.csv").unwrap();
        assert!(zip.room_for(256 * 1024));
        // Anything that could carry the entry or the archive past 4 GiB.
        assert!(!zip.room_for(ZIP32_MAX as usize));
        assert!(!zip.room_for((ZIP32_MAX - TAIL_RESERVE) as usize));
        // The largest chunk that fits still leaves the reserve intact.
        let fits = ZIP32_MAX - TAIL_RESERVE - zip.len();
        let chunk = (fits - 64) * 8 / 9;
        assert!(zip.room_for(chunk as usize));
        assert!(zip.len() + deflate_ceiling(chunk) + TAIL_RESERVE <= ZIP32_MAX);

        // A field past the limit is an error, never a wrapped number.
        assert!(zip32(ZIP32_MAX, "x").is_ok());
        assert!(zip32(ZIP32_MAX + 1, "x").is_err());
        assert!(zip32(u64::MAX, "x").is_err());
    }

    #[test]
    fn a_closed_archive_takes_nothing_more() {
        let mut zip = ZipWriter::new(stamp());
        zip.finish().unwrap();
        let empty = zip.take();
        // An archive with no files is just the end record.
        assert_eq!(empty.len(), 22);
        assert_eq!(le32(&empty, 0), END_OF_DIRECTORY);
        assert!(zip.start("late.csv").is_err());
        assert!(zip.finish().is_ok(), "finishing twice is harmless");
        assert!(zip.take().is_empty());
    }
}
