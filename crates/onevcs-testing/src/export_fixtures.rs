//! Deterministic, generated export carriers shared with boundary consumers.

use std::io::{Cursor, Write};

use flate2::{write::GzEncoder, Compression};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

/// The one location in which a sample's private term is planted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// JPEG EXIF image description.
    JpegExif,
    /// JPEG XMP packet.
    JpegXmp,
    /// PNG textual metadata.
    PngText,
    /// Binary STL's 80-byte header.
    StlHeader,
    /// 3MF model XML.
    ThreeMfMember,
    /// A 3MF ZIP member's name.
    ThreeMfMemberName,
    /// Gzip's G-code payload.
    GzipPayload,
    /// Markdown prose.
    MarkdownText,
}

/// A term and its single carrier in [`sample_set`].
#[derive(Debug, Clone)]
pub struct Plant {
    /// The private text.
    pub term: String,
    /// Its only location.
    pub carrier: Carrier,
}

/// A payload's character encoding.
#[derive(Debug, Clone, Copy)]
pub enum Encoding {
    /// UTF-8, without a NUL.
    Utf8,
    /// UTF-16 little endian.
    Utf16Le,
    /// UTF-16 big endian.
    Utf16Be,
}

/// The ZIP representation used by a layer.
#[derive(Debug, Clone, Copy)]
pub enum ZipForm {
    /// Uncompressed member.
    Stored,
    /// Deflated member.
    Deflate,
    /// Stored member with ZIP64 sizes in both headers.
    Zip64,
}

/// One wrapper, applied innermost first.
#[derive(Debug, Clone)]
pub enum Layer {
    /// A gzip member.
    Gzip,
    /// A ZIP member with the given name.
    Zip {
        /// Member path.
        member: String,
        /// Compression and header form.
        form: ZipForm,
    },
    /// Text is the member name; the member itself is clean XML.
    ZipName {
        /// Compression and header form.
        form: ZipForm,
    },
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).expect("memory gzip write");
    encoder.finish().expect("memory gzip finish")
}

fn archive(members: &[(&str, &[u8])], form: ZipForm) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let method = match form {
        ZipForm::Deflate => CompressionMethod::Deflated,
        ZipForm::Stored | ZipForm::Zip64 => CompressionMethod::Stored,
    };
    let options = SimpleFileOptions::default()
        .compression_method(method)
        .large_file(matches!(form, ZipForm::Zip64));
    for (name, bytes) in members {
        writer
            .start_file(*name, options)
            .expect("memory ZIP member");
        writer.write_all(bytes).expect("memory ZIP write");
    }
    writer.finish().expect("memory ZIP finish").into_inner()
}

/// Encode `text`, then wrap it innermost first. UTF-8 input must have no NUL.
pub fn layered(text: &str, encoding: Encoding, layers: &[Layer]) -> Vec<u8> {
    assert!(!text.contains('\0'), "fixture text must have no NUL");
    let mut bytes = match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        Encoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
    };
    for layer in layers {
        bytes = match layer {
            Layer::Gzip => gzip(&bytes),
            Layer::Zip { member, form } => archive(&[(member, &bytes)], *form),
            Layer::ZipName { form } => archive(&[(text, b"<model/>\n")], *form),
        };
    }
    bytes
}

/// Concatenated gzip members, including a separate member for each text.
pub fn gzip_members(texts: &[&str]) -> Vec<u8> {
    texts
        .iter()
        .flat_map(|text| gzip(text.as_bytes()))
        .collect()
}

/// A member the generic export decoder cannot fully read.
#[derive(Debug, Clone, Copy)]
pub enum UnreadableMember {
    /// ZIP's encrypted flag.
    Encrypted,
    /// ZIP method 14.
    Lzma,
}

/// One clean XML member and an unreadable member named `visible`.
///
/// The stored payload remains visible to raw-byte scanning. Changing both headers
/// makes this a real unsupported-method/encryption boundary, not a decoder double.
pub fn zip_with_unreadable_member(kind: UnreadableMember, visible: &str) -> Vec<u8> {
    let mut bytes = archive(
        &[
            ("clean.xml", b"<model/>\n"),
            ("visible", visible.as_bytes()),
        ],
        ZipForm::Stored,
    );
    let mut offset = 0;
    while offset + 46 <= bytes.len() {
        let (name_offset, flag_offset, method_offset) = match &bytes[offset..offset + 4] {
            b"PK\x03\x04" => (30, 6, 8),
            b"PK\x01\x02" => (46, 8, 10),
            _ => {
                offset += 1;
                continue;
            }
        };
        if bytes.get(offset + name_offset..offset + name_offset + 7) == Some(b"visible") {
            match kind {
                UnreadableMember::Encrypted => bytes[offset + flag_offset] |= 1,
                UnreadableMember::Lzma => {
                    bytes[offset + method_offset..offset + method_offset + 2]
                        .copy_from_slice(&14u16.to_le_bytes());
                }
            }
        }
        offset += 4;
    }
    bytes
}

/// Cheaply generate a concatenated gzip decoding to exactly `decoded_len` zeros.
pub fn gzip_of_zeros(decoded_len: u64) -> Vec<u8> {
    if decoded_len == 0 {
        return gzip(&[]);
    }
    const MEMBER: usize = 1024 * 1024;
    let zeros = vec![0; MEMBER];
    let member = gzip(&zeros);
    let mut bytes = Vec::new();
    for _ in 0..decoded_len / MEMBER as u64 {
        bytes.extend_from_slice(&member);
    }
    let tail = (decoded_len % MEMBER as u64) as usize;
    if tail != 0 {
        bytes.extend_from_slice(&gzip(&zeros[..tail]));
    }
    bytes
}

/// Patterns for the three opaque files in [`sample_set`].
pub fn sample_opaque_patterns() -> Vec<String> {
    ["*.jpg", "*.png", "*.stl"].map(String::from).to_vec()
}

/// Git LFS attributes for the sample's binary, ZIP and gzip files.
pub fn lfs_attributes() -> String {
    ["*.jpg", "*.png", "*.stl", "*.3mf", "*.gz"]
        .map(|pattern| format!("{pattern} filter=lfs diff=lfs merge=lfs -text\n"))
        .concat()
}

fn planted(plant: Option<&Plant>, carrier: Carrier) -> &str {
    plant
        .filter(|plant| plant.carrier == carrier)
        .map_or("", |plant| plant.term.as_str())
}

fn jpeg_segment(bytes: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    bytes.extend_from_slice(&[0xff, marker]);
    bytes.extend_from_slice(
        &u16::try_from(payload.len() + 2)
            .expect("JPEG metadata size")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(payload);
}

fn png_chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(payload);
    let mut crc = !0u32;
    for byte in kind.iter().chain(payload) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    bytes.extend_from_slice(&(!crc).to_be_bytes());
}

/// Generate notes, JPEG/PNG frames, binary STL, XML-only 3MF and gzipped G-code.
/// A plant appears in exactly its selected carrier.
pub fn sample_set(plant: Option<&Plant>) -> Vec<(String, Vec<u8>)> {
    let mut jpeg = vec![0xff, 0xd8];
    // A little-endian TIFF IFD containing ImageDescription (ASCII).
    let description = format!("frame {}\0", planted(plant, Carrier::JpegExif));
    let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0\x0e\x01\x02\0".to_vec();
    exif.extend_from_slice(&(description.len() as u32).to_le_bytes());
    exif.extend_from_slice(&26u32.to_le_bytes());
    exif.extend_from_slice(&0u32.to_le_bytes());
    exif.extend_from_slice(description.as_bytes());
    jpeg_segment(&mut jpeg, 0xe1, &exif);
    let xmp = format!(
        "http://ns.adobe.com/xap/1.0/\0<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">{}</x:xmpmeta>",
        planted(plant, Carrier::JpegXmp)
    );
    jpeg_segment(&mut jpeg, 0xe1, xmp.as_bytes());
    // A baseline grayscale 1x1 image: constant quantization, one DC-zero
    // code and one AC end-of-block code. The entropy byte is those two
    // zero bits followed by the JPEG-required one-bit padding.
    let mut quantization = vec![0];
    quantization.extend_from_slice(&[1; 64]);
    jpeg_segment(&mut jpeg, 0xdb, &quantization);
    jpeg_segment(&mut jpeg, 0xc0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0]);
    for table in [0, 0x10] {
        let mut huffman = vec![table, 1];
        huffman.extend_from_slice(&[0; 15]);
        huffman.push(0);
        jpeg_segment(&mut jpeg, 0xc4, &huffman);
    }
    jpeg_segment(&mut jpeg, 0xda, &[1, 1, 0, 0, 63, 0]);
    jpeg.extend_from_slice(&[0x3f, 0xff, 0xd9]);

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 0, 0, 0, 0]);
    png_chunk(
        &mut png,
        b"tEXt",
        format!("Description\0frame {}", planted(plant, Carrier::PngText)).as_bytes(),
    );
    png_chunk(
        &mut png,
        b"IDAT",
        &[0x78, 0x9c, 0x63, 0x60, 0, 0, 0, 2, 0, 1],
    );
    png_chunk(&mut png, b"IEND", &[]);

    let header = format!("Binary triangle {}", planted(plant, Carrier::StlHeader));
    assert!(header.len() <= 80, "STL header term too long");
    let mut stl = vec![0; 80];
    stl[..header.len()].copy_from_slice(header.as_bytes());
    stl.extend_from_slice(&1u32.to_le_bytes());
    for value in [0f32, 0., 1., 0., 0., 0., 1., 0., 0., 0., 1., 0.] {
        stl.extend_from_slice(&value.to_le_bytes());
    }
    stl.extend_from_slice(&0u16.to_le_bytes());
    let xml = format!("<?xml version=\"1.0\"?><model xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\" unit=\"millimeter\"><metadata name=\"Title\">{}</metadata><resources><object id=\"1\" type=\"model\"><mesh><vertices><vertex x=\"0\" y=\"0\" z=\"0\"/><vertex x=\"1\" y=\"0\" z=\"0\"/><vertex x=\"0\" y=\"1\" z=\"0\"/></vertices><triangles><triangle v1=\"0\" v2=\"1\" v3=\"2\"/></triangles></mesh></object></resources><build><item objectid=\"1\"/></build></model>\n", planted(plant, Carrier::ThreeMfMember));
    let relationships = r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#;
    let name = format!(
        "3D/{}3dmodel.model",
        planted(plant, Carrier::ThreeMfMemberName)
    );
    vec![
        (
            "notes.md".into(),
            format!(
                "# Print evidence\nA clean sample. {}\n",
                planted(plant, Carrier::MarkdownText)
            )
            .into_bytes(),
        ),
        ("frame.jpg".into(), jpeg),
        ("frame.png".into(), png),
        ("model.stl".into(), stl),
        (
            "model.3mf".into(),
            archive(
                &[
                    ("[Content_Types].xml", br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>"#),
                    ("_rels/.rels", relationships.as_bytes()),
                    (&name, xml.as_bytes()),
                ],
                ZipForm::Deflate,
            ),
        ),
        (
            "print.gcode.gz".into(),
            gzip(
                format!(
                    "; print evidence {}\nG28\nG1 X10 Y10\n",
                    planted(plant, Carrier::GzipPayload)
                )
                .as_bytes(),
            ),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    fn has_zip64(extra: &[u8]) -> bool {
        let mut at = 0;
        while at + 4 <= extra.len() {
            let size = usize::from(u16_at(extra, at + 2));
            if u16_at(extra, at) == 1 {
                return size >= 16;
            }
            at += 4 + size;
        }
        false
    }

    #[test]
    fn zip_forms_read_with_independent_reader_and_zip64_headers() {
        // Python's standard library is independent of the Rust ZIP writer used
        // by the fixtures. Header checks additionally distinguish a genuinely
        // ZIP64 member from a tiny stored member with the same decoded bytes.
        for (form, method) in [
            (ZipForm::Stored, 0),
            (ZipForm::Deflate, 8),
            (ZipForm::Zip64, 0),
        ] {
            let bytes = layered(
                "fixture payload",
                Encoding::Utf8,
                &[Layer::Zip {
                    member: "3D/model.xml".into(),
                    form,
                }],
            );
            let directory = bytes
                .windows(4)
                .position(|window| window == b"PK\x01\x02")
                .unwrap();
            assert_eq!(u16_at(&bytes, 8), method);
            assert_eq!(u16_at(&bytes, directory + 10), method);
            if matches!(form, ZipForm::Zip64) {
                let local_extra = 30 + usize::from(u16_at(&bytes, 26));
                assert!(has_zip64(
                    &bytes[local_extra..local_extra + usize::from(u16_at(&bytes, 28))]
                ));
                let central_extra = directory + 46 + usize::from(u16_at(&bytes, directory + 28));
                assert!(has_zip64(
                    &bytes[central_extra
                        ..central_extra + usize::from(u16_at(&bytes, directory + 30))]
                ));
                assert_eq!(u32_at(&bytes, 18), u32::MAX);
                assert_eq!(u32_at(&bytes, directory + 20), u32::MAX);
            }
            let scratch = tempfile::tempdir().unwrap();
            let path = scratch.path().join("fixture.zip");
            std::fs::write(&path, bytes).unwrap();
            let output = std::process::Command::new("python3")
                .args(["-c", "import sys,zipfile; z=zipfile.ZipFile(sys.argv[1]); assert z.namelist()==['3D/model.xml']; assert z.read('3D/model.xml')==b'fixture payload'; assert z.getinfo('3D/model.xml').compress_type==int(sys.argv[2])"])
                .arg(path).arg(method.to_string()).output().expect("python3 independent ZIP reader");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn decoded_sample(name: &str, bytes: &[u8]) -> Vec<u8> {
        if name.ends_with(".gz") {
            let mut decoded = Vec::new();
            flate2::read::MultiGzDecoder::new(bytes)
                .read_to_end(&mut decoded)
                .unwrap();
            decoded
        } else if name.ends_with(".3mf") {
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            let mut decoded = Vec::new();
            for index in 0..archive.len() {
                let mut member = archive.by_index(index).unwrap();
                decoded.extend_from_slice(member.name().as_bytes());
                member.read_to_end(&mut decoded).unwrap();
            }
            decoded
        } else {
            bytes.to_vec()
        }
    }

    #[test]
    fn each_plant_occurs_only_in_its_carrier() {
        let term = "private-fixture-orchid";
        let cases = [
            (Carrier::JpegExif, "frame.jpg"),
            (Carrier::JpegXmp, "frame.jpg"),
            (Carrier::PngText, "frame.png"),
            (Carrier::StlHeader, "model.stl"),
            (Carrier::ThreeMfMember, "model.3mf"),
            (Carrier::ThreeMfMemberName, "model.3mf"),
            (Carrier::GzipPayload, "print.gcode.gz"),
            (Carrier::MarkdownText, "notes.md"),
        ];
        for (carrier, expected) in cases {
            let plant = Plant {
                term: term.into(),
                carrier,
            };
            let planted = sample_set(Some(&plant));
            let clean = sample_set(None);
            assert_eq!(planted.len(), 6);
            for ((name, bytes), (clean_name, clean_bytes)) in planted.iter().zip(&clean) {
                assert_eq!(name, clean_name);
                let decoded = decoded_sample(name, bytes);
                let count = decoded
                    .windows(term.len())
                    .filter(|window| *window == term.as_bytes())
                    .count();
                assert_eq!(count, usize::from(name == expected), "{carrier:?}: {name}");
                if name != expected {
                    assert_eq!(bytes, clean_bytes);
                }
            }
        }
    }

    #[test]
    fn encodings_and_wrappers_preserve_payload_and_order() {
        for encoding in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be] {
            let expected = layered("Print 🪻", encoding, &[]);
            let bytes = layered(
                "Print 🪻",
                encoding,
                &[
                    Layer::Gzip,
                    Layer::Zip {
                        member: "inner.gz".into(),
                        form: ZipForm::Deflate,
                    },
                    Layer::Zip {
                        member: "outer.zip".into(),
                        form: ZipForm::Zip64,
                    },
                ],
            );
            let mut outer = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            let mut middle = Vec::new();
            outer
                .by_name("outer.zip")
                .unwrap()
                .read_to_end(&mut middle)
                .unwrap();
            let mut middle = zip::ZipArchive::new(Cursor::new(middle)).unwrap();
            let mut compressed = Vec::new();
            middle
                .by_name("inner.gz")
                .unwrap()
                .read_to_end(&mut compressed)
                .unwrap();
            let mut decoded = Vec::new();
            flate2::read::MultiGzDecoder::new(compressed.as_slice())
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(decoded, expected);
        }
        for form in [ZipForm::Stored, ZipForm::Deflate, ZipForm::Zip64] {
            let bytes = layered("private-name", Encoding::Utf8, &[Layer::ZipName { form }]);
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            let mut content = String::new();
            archive
                .by_name("private-name")
                .unwrap()
                .read_to_string(&mut content)
                .unwrap();
            assert_eq!(content, "<model/>\n");
        }
    }

    #[test]
    fn concatenated_gzip_and_repeated_zeros_decode_completely() {
        let bytes = gzip_members(&["first", "second", "third"]);
        let mut decoded = String::new();
        flate2::read::MultiGzDecoder::new(bytes.as_slice())
            .read_to_string(&mut decoded)
            .unwrap();
        assert_eq!(decoded, "firstsecondthird");
        for size in [0, 1, 2 * 1024 * 1024 + 7] {
            let bytes = gzip_of_zeros(size);
            let mut decoded = Vec::new();
            flate2::read::MultiGzDecoder::new(bytes.as_slice())
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(decoded.len() as u64, size);
            assert!(decoded.iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn unreadable_members_keep_visible_payload_and_clean_member() {
        for kind in [UnreadableMember::Encrypted, UnreadableMember::Lzma] {
            let bytes = zip_with_unreadable_member(kind, "private-visible");
            assert!(bytes.windows(15).any(|window| window == b"private-visible"));
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
            let mut clean = String::new();
            archive
                .by_name("clean.xml")
                .unwrap()
                .read_to_string(&mut clean)
                .unwrap();
            assert_eq!(clean, "<model/>\n");
            assert!(archive.by_name("visible").is_err());
        }
    }
}
