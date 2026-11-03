//! Generic extraction only: printable runs, gzip and ZIP. No file-type inference.

use std::io::{Cursor, Read};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

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

pub(super) struct Extraction {
    pub readable: bool,
    pub bounded: bool,
}

/// A run's original location, formatted only if the matcher finds a term.
#[derive(Clone, Copy)]
pub(super) struct Location<'a> {
    pub path: &'a str,
    pub offset: usize,
    pub encoding: &'static str,
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
    check: impl FnMut(bool, Location<'_>, &str),
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
    Extraction {
        readable,
        bounded: reader.bounded,
    }
}

impl<F: FnMut(bool, Location<'_>, &str), L: Fn(&str) -> String> Reader<'_, F, L> {
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
                let member = format!("{at}!zip:{}", (self.locate)(raw.name()));
                (self.check)(
                    true,
                    Location {
                        path: &member,
                        offset: 0,
                        encoding: "",
                    },
                    raw.name(),
                );
                let supported = !raw.encrypted()
                    && matches!(
                        raw.compression(),
                        zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated
                    );
                let directory = raw.is_dir();
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
                if directory {
                    continue;
                }
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
                false,
                Location {
                    path: at,
                    offset: 0,
                    encoding: "",
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
        for little in [true, false] {
            for alignment in 0..2 {
                self.utf16(bytes, at, alignment, little);
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
                        false,
                        Location {
                            path: at,
                            offset: base + start,
                            encoding: "",
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
                false,
                Location {
                    path: at,
                    offset: base + start,
                    encoding: "",
                },
                &text[start..],
            );
        }
    }

    fn utf16(&mut self, bytes: &[u8], at: &str, alignment: usize, little: bool) {
        let mut run = String::new();
        let mut count = 0;
        let mut start = alignment;
        let mut offset = alignment;
        let suffix = if little { "utf16le" } else { "utf16be" };
        while offset + 1 < bytes.len() {
            let unit = if little {
                u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
            } else {
                u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
            };
            let (character, width) =
                if (0xd800..=0xdbff).contains(&unit) && offset + 3 < bytes.len() {
                    let next = if little {
                        u16::from_le_bytes([bytes[offset + 2], bytes[offset + 3]])
                    } else {
                        u16::from_be_bytes([bytes[offset + 2], bytes[offset + 3]])
                    };
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
                            false,
                            Location {
                                path: at,
                                offset: start,
                                encoding: suffix,
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
                false,
                Location {
                    path: at,
                    offset: start,
                    encoding: suffix,
                },
                &run,
            );
        }
    }
}
