//! Generic extraction only: printable runs, gzip and ZIP. No file-type inference.

use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use crate::boundary::Surface;

/// Every layer counts, including the outermost file at depth zero.
pub(super) const MAX_DEPTH: usize = 8;
pub(super) const MAX_FILE_DECODED_BYTES: u64 = 256 * 1024 * 1024;
pub(super) const MAX_EXPORT_DECODED_BYTES: u64 = 1024 * 1024 * 1024;
pub(super) const READ_BUFFER_BYTES: usize = 64 * 1024;

/// Shared accounting prevents parallel files from each spending the export cap.
#[derive(Default)]
pub(super) struct Budget {
    pub bytes: AtomicU64,
    stopped: AtomicBool,
    turn: Mutex<usize>,
    ready: Condvar,
}

impl Budget {
    fn wait(&self, index: usize) {
        let mut turn = self.turn.lock().expect("export budget lock");
        while *turn != index {
            turn = self.ready.wait(turn).expect("export budget lock");
        }
    }

    fn finish(&self, index: usize) {
        let mut turn = self.turn.lock().expect("export budget lock");
        while *turn != index {
            turn = self.ready.wait(turn).expect("export budget lock");
        }
        *turn += 1;
        self.ready.notify_all();
    }
}

/// Whether every byte could be interpreted, or why acceptance cannot suffice.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Extraction {
    Readable,
    Opaque,
    BoundExceeded,
}

#[derive(Clone, Copy)]
pub(super) enum Endian {
    Little,
    Big,
}

impl Endian {
    fn unit(self, bytes: &[u8], offset: usize) -> u16 {
        let pair = [bytes[offset], bytes[offset + 1]];
        match self {
            Self::Little => u16::from_le_bytes(pair),
            Self::Big => u16::from_be_bytes(pair),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Encoding {
    Utf8,
    Utf16(Endian),
}

impl Encoding {
    pub fn suffix(self) -> Option<&'static str> {
        match self {
            Self::Utf8 => None,
            Self::Utf16(Endian::Little) => Some("utf16le"),
            Self::Utf16(Endian::Big) => Some("utf16be"),
        }
    }
}

/// A run's original location, formatted only if the matcher finds a term.
#[derive(Clone, Copy)]
pub(super) struct Location<'a> {
    pub path: &'a str,
    pub offset: usize,
    pub encoding: Encoding,
}

struct Reader<'a, F, L> {
    budget: &'a Budget,
    index: usize,
    bytes: u64,
    bounded: bool,
    check: F,
    locate: L,
}

/// Scan raw bytes even when a container later proves unreadable.
pub(super) fn inspect(
    bytes: &[u8],
    at: &str,
    budget: &Budget,
    index: Option<usize>,
    check: impl FnMut(Surface, Location<'_>, &str),
    locate: impl Fn(&str) -> String,
) -> Extraction {
    let mut reader = Reader {
        budget,
        index: index.unwrap_or(0),
        bytes: 0,
        bounded: false,
        check,
        locate,
    };
    let at = (reader.locate)(at);
    let readable = reader.layer(bytes, &at, 0);
    if let Some(index) = index {
        budget.finish(index);
    }
    if reader.bounded {
        Extraction::BoundExceeded
    } else if readable {
        Extraction::Readable
    } else {
        Extraction::Opaque
    }
}

impl<F: FnMut(Surface, Location<'_>, &str), L: Fn(&str) -> String> Reader<'_, F, L> {
    fn account(&mut self, count: usize) -> bool {
        // Account the actual read, including the single buffer crossing a cap.
        // Containers spend this budget in path order, so no other decoder can
        // concurrently read another buffer beyond the export limit.
        let count = count as u64;
        self.bytes += count;
        let total = self.budget.bytes.fetch_add(count, Ordering::Relaxed) + count;
        if total > MAX_EXPORT_DECODED_BYTES {
            self.budget.stopped.store(true, Ordering::Relaxed);
            self.bounded = true;
            return false;
        }
        if self.bytes > MAX_FILE_DECODED_BYTES {
            self.bounded = true;
            return false;
        }
        true
    }

    fn decode(&mut self, mut stream: impl Read, at: &str) -> Option<Vec<u8>> {
        self.budget.wait(self.index);
        if self.budget.stopped.load(Ordering::Relaxed) {
            self.bounded = true;
            return None;
        }
        let mut result = Vec::new();
        let mut buffer = [0; READ_BUFFER_BYTES];
        loop {
            let size = match stream.read(&mut buffer) {
                Ok(0) => return Some(result),
                Ok(size) => size,
                Err(_) => {
                    self.scan(&result, at);
                    return None;
                }
            };
            result.extend_from_slice(&buffer[..size]);
            if !self.account(size) {
                self.scan(&result, at);
                return None;
            }
        }
    }

    fn layer(&mut self, bytes: &[u8], at: &str, depth: usize) -> bool {
        self.scan(bytes, at);
        if bytes.starts_with(&[0x1f, 0x8b]) {
            if depth >= MAX_DEPTH {
                self.bounded = true;
                return false;
            }
            let decoder = flate2::read::MultiGzDecoder::new(bytes);
            return self
                .decode(decoder, &format!("{at}!gzip"))
                .is_some_and(|decoded| self.layer(&decoded, &format!("{at}!gzip"), depth + 1));
        }
        if bytes.starts_with(b"PK\x03\x04") {
            if depth >= MAX_DEPTH {
                self.bounded = true;
                return false;
            }
            let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(bytes)) else {
                return false;
            };
            let mut readable = true;
            for index in 0..archive.len() {
                // Read the name independently of decompression/encryption support.
                let Ok(raw) = archive.by_index_raw(index) else {
                    readable = false;
                    continue;
                };
                // llmlint: ignore[changed_behavior_has_e2e] zip 9.3 name() decodes valid UTF-8 directly and every other byte sequence through total CP437 conversion; its Result error is unreachable from an archive. Keep the defensive opaque refusal for a future decoder that can fail, rather than fabricate a mocked ZIP error. Existing real-archive journeys cover member names and continuation past unreadable members.
                let Ok(name) = raw.name() else {
                    readable = false;
                    continue;
                };
                let member = format!("{at}!zip:{}", (self.locate)(&name));
                (self.check)(
                    Surface::Path,
                    Location {
                        path: &member,
                        offset: 0,
                        encoding: Encoding::Utf8,
                    },
                    &name,
                );
                let supported = !raw.encrypted()
                    && matches!(
                        raw.compression(),
                        zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
                    );
                if !supported {
                    // A raw member remains readable as bytes even when its
                    // compression/encryption cannot be decoded. Scan its own
                    // extent, without adjoining the next ZIP header to its text.
                    if let Some(bytes) = self.decode(raw, &member) {
                        self.scan(&bytes, &member);
                    }
                    readable = false;
                    if self.bounded {
                        break;
                    }
                    continue;
                }
                drop(raw);
                let Ok(file) = archive.by_index(index) else {
                    readable = false;
                    continue;
                };
                match self.decode(file, &member) {
                    Some(decoded) => readable &= self.layer(&decoded, &member, depth + 1),
                    None => readable = false,
                }
                if self.bounded {
                    break;
                }
            }
            return readable;
        }
        !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok()
    }

    fn scan(&mut self, bytes: &[u8], at: &str) {
        let whole = std::str::from_utf8(bytes)
            .ok()
            .filter(|_| !bytes.contains(&0));
        if let Some(text) = whole {
            (self.check)(
                Surface::Content,
                Location {
                    path: at,
                    offset: 0,
                    encoding: Encoding::Utf8,
                },
                text,
            );
        }
        // UTF-8 decoding resumes after each invalid sequence; byte offsets always
        // address the original layer, never a lossy replacement string.
        let mut offset = if whole.is_some() { bytes.len() } else { 0 };
        while offset < bytes.len() {
            match std::str::from_utf8(&bytes[offset..]) {
                Ok(text) => {
                    self.utf8(text, at, offset);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if let Ok(text) = std::str::from_utf8(&bytes[offset..offset + valid]) {
                        self.utf8(text, at, offset);
                    }
                    offset += valid + error.error_len().unwrap_or(bytes.len() - offset - valid);
                }
            }
        }
        for endian in [Endian::Little, Endian::Big] {
            for alignment in 0..2 {
                self.utf16(bytes, at, alignment, endian);
            }
        }
    }

    fn utf8(&mut self, text: &str, at: &str, base: usize) {
        let mut start = 0;
        let mut count = 0;
        for (offset, character) in text.char_indices() {
            if character.is_control() {
                if count >= 4 {
                    (self.check)(
                        Surface::Content,
                        Location {
                            path: at,
                            offset: base + start,
                            encoding: Encoding::Utf8,
                        },
                        &text[start..offset],
                    );
                }
                count = 0;
                start = offset + character.len_utf8();
            } else {
                count += 1;
            }
        }
        if count >= 4 {
            (self.check)(
                Surface::Content,
                Location {
                    path: at,
                    offset: base + start,
                    encoding: Encoding::Utf8,
                },
                &text[start..],
            );
        }
    }

    fn utf16(&mut self, bytes: &[u8], at: &str, alignment: usize, endian: Endian) {
        let mut run = String::new();
        let mut count = 0;
        let mut start = alignment;
        let mut offset = alignment;
        let encoding = Encoding::Utf16(endian);
        while offset + 1 < bytes.len() {
            let unit = endian.unit(bytes, offset);
            let (character, width) =
                if (0xd800..=0xdbff).contains(&unit) && offset + 3 < bytes.len() {
                    let next = endian.unit(bytes, offset + 2);
                    if (0xdc00..=0xdfff).contains(&next) {
                        (
                            char::decode_utf16([unit, next]).next().and_then(Result::ok),
                            4,
                        )
                    } else {
                        (None, 2)
                    }
                } else {
                    (char::from_u32(u32::from(unit)), 2)
                };
            match character.filter(|character| !character.is_control()) {
                Some(character) => {
                    if count == 0 {
                        start = offset;
                    }
                    run.push(character);
                    count += 1;
                }
                None => {
                    if count >= 4 {
                        (self.check)(
                            Surface::Content,
                            Location {
                                path: at,
                                offset: start,
                                encoding,
                            },
                            &run,
                        );
                    }
                    run.clear();
                    count = 0;
                }
            }
            offset += width;
        }
        if count >= 4 {
            (self.check)(
                Surface::Content,
                Location {
                    path: at,
                    offset: start,
                    encoding,
                },
                &run,
            );
        }
    }
}
