//! The spec-v1 pointer grammar shared by export and branch publication.

pub(crate) const MAX_BYTES: usize = 1024;

pub(crate) struct Pointer {
    pub(crate) oid: String,
    pub(crate) size: u64,
}

pub(crate) fn parse(bytes: &[u8]) -> Option<Pointer> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "version https://git-lfs.github.com/spec/v1" {
        return None;
    }
    let oid = lines.next()?.strip_prefix("oid sha256:")?;
    if oid.len() != 64
        || !oid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let size = lines.next()?.strip_prefix("size ")?.parse().ok()?;
    if lines.next().is_some() {
        return None;
    }
    Some(Pointer {
        oid: oid.to_owned(),
        size,
    })
}
