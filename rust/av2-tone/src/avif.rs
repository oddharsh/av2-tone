//! The AVIF container, as far as an AV2 still needs it: find the primary item,
//! check it is `av02`, and gather its extents into one OBU stream. A port of
//! `av2Item` in ../../fast-decode/check.ts, with every offset checked.

use crate::error::{Error, Result};

struct Box<'a> {
    kind: [u8; 4],
    body: &'a [u8],
}

fn be(bytes: &[u8], at: usize, n: usize) -> Result<u64> {
    let s = at.checked_add(n).and_then(|end| bytes.get(at..end)).ok_or(Error::Truncated("container field"))?;
    Ok(s.iter().fold(0u64, |v, &b| (v << 8) | u64::from(b)))
}

fn boxes(data: &[u8]) -> Result<Vec<Box<'_>>> {
    let mut out = Vec::new();
    let mut o = 0usize;
    while data.len().saturating_sub(o) >= 8 {
        let mut size = be(data, o, 4)?;
        let mut hdr = 8usize;
        if size == 1 {
            size = be(data, o + 8, 8)?;
            hdr = 16;
        } else if size == 0 {
            size = (data.len() - o) as u64;
        }
        let size = usize::try_from(size).map_err(|_| Error::Invalid("box size"))?;
        let end = o.checked_add(size).ok_or(Error::Invalid("box size"))?;
        if size < hdr || end > data.len() {
            return Err(Error::Truncated("box"));
        }
        let mut kind = [0u8; 4];
        kind.copy_from_slice(data.get(o + 4..o + 8).ok_or(Error::Truncated("box type"))?);
        out.push(Box { kind, body: data.get(o + hdr..end).ok_or(Error::Truncated("box"))? });
        o = end;
    }
    Ok(out)
}

fn find<'a>(list: &'a [Box<'a>], kind: &[u8; 4]) -> Option<&'a Box<'a>> {
    list.iter().find(|b| &b.kind == kind)
}

/// A cursor over a full box's fields.
struct Fields<'a> {
    data: &'a [u8],
    at: usize,
}

impl Fields<'_> {
    fn read(&mut self, n: usize) -> Result<u64> {
        let v = be(self.data, self.at, n)?;
        self.at += n;
        Ok(v)
    }
}

/// The primary item's bytes, extents concatenated: the AV2 OBU stream.
pub fn primary_item(file: &[u8]) -> Result<Vec<u8>> {
    let top = boxes(file)?;
    let meta = find(&top, b"meta").ok_or(Error::NotAv2)?;
    let kids = boxes(meta.body.get(4..).ok_or(Error::Truncated("meta"))?)?;
    let pitm = find(&kids, b"pitm").ok_or(Error::NotAv2)?;
    let iinf = find(&kids, b"iinf").ok_or(Error::NotAv2)?;
    let iloc = find(&kids, b"iloc").ok_or(Error::NotAv2)?;

    let pitm_version = be(pitm.body, 0, 1)?;
    let primary = be(pitm.body, 4, if pitm_version == 0 { 2 } else { 4 })?;

    // iinf: version, flags, entry count, then infe boxes.
    let iinf_version = be(iinf.body, 0, 1)?;
    let entries = iinf.body.get(4 + if iinf_version == 0 { 2 } else { 4 }..).ok_or(Error::Truncated("iinf"))?;
    let mut item_type = None;
    for infe in boxes(entries)?.iter().filter(|b| &b.kind == b"infe") {
        let version = be(infe.body, 0, 1)?;
        if version < 2 {
            continue; // infe before version 2 carries no item_type
        }
        let wide = version >= 3;
        let id = be(infe.body, 4, if wide { 4 } else { 2 })?;
        if id == primary {
            let at = 4 + if wide { 4 } else { 2 } + 2;
            let t = infe.body.get(at..at + 4).ok_or(Error::Truncated("infe"))?;
            item_type = <[u8; 4]>::try_from(t).ok();
        }
    }
    if item_type != Some(*b"av02") {
        return Err(Error::NotAv2);
    }

    let mut f = Fields { data: iloc.body, at: 0 };
    let version = f.read(1)?;
    f.read(3)?; // flags
    let sizes = f.read(1)?;
    let (off_size, len_size) = ((sizes >> 4) as usize, (sizes & 15) as usize);
    let sizes = f.read(1)?;
    let base_size = (sizes >> 4) as usize;
    let idx_size = if version >= 1 { (sizes & 15) as usize } else { 0 };
    let id_size = if version < 2 { 2 } else { 4 };
    let count = f.read(id_size)?;
    let mut out = Vec::new();
    for _ in 0..count {
        let id = f.read(id_size)?;
        if version >= 1 {
            f.read(2)?; // construction method
        }
        f.read(2)?; // data reference index
        let base = f.read(base_size)?;
        let extents = f.read(2)?;
        for _ in 0..extents {
            if version >= 1 && idx_size > 0 {
                f.read(idx_size)?;
            }
            let off = f.read(off_size)?;
            let len = f.read(len_size)?;
            if id == primary {
                let start = base.checked_add(off).ok_or(Error::Invalid("iloc offset"))?;
                let end = start.checked_add(len).ok_or(Error::Invalid("iloc length"))?;
                let (start, end) = (
                    usize::try_from(start).map_err(|_| Error::Invalid("iloc offset"))?,
                    usize::try_from(end).map_err(|_| Error::Invalid("iloc length"))?,
                );
                out.extend_from_slice(file.get(start..end).ok_or(Error::Truncated("item extent"))?);
            }
        }
    }
    if out.is_empty() {
        return Err(Error::Invalid("primary item has no data"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    /// A minimal AVIF: one av02 item whose two extents sit in mdat.
    fn file(item_type: &[u8; 4]) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"avifmif1");
        let pitm = bx(b"pitm", &[0, 0, 0, 0, 0, 1]);
        let mut infe_body = vec![2, 0, 0, 0, 0, 1, 0, 0];
        infe_body.extend_from_slice(item_type);
        infe_body.push(0);
        let infe = bx(b"infe", &infe_body);
        let mut iinf_body = vec![0, 0, 0, 0, 0, 1];
        iinf_body.extend_from_slice(&infe);
        let iinf = bx(b"iinf", &iinf_body);
        let iloc_len = 8 + 14 + 2 * 8; // header, fixed fields, two extents
        let meta_len = 8 + 4 + pitm.len() + iinf.len() + iloc_len;
        let mdat_start = (ftyp.len() + meta_len + 8) as u32;
        // version 0, offset 4 bytes, length 4 bytes, no base offset.
        let mut iloc_body = vec![0, 0, 0, 0, 0x44, 0x00, 0, 1, 0, 1, 0, 0, 0, 2];
        for (off, len) in [(mdat_start, 3u32), (mdat_start + 5, 2)] {
            iloc_body.extend_from_slice(&off.to_be_bytes());
            iloc_body.extend_from_slice(&len.to_be_bytes());
        }
        let iloc = bx(b"iloc", &iloc_body);
        let mut meta_body = vec![0, 0, 0, 0];
        meta_body.extend_from_slice(&pitm);
        meta_body.extend_from_slice(&iinf);
        meta_body.extend_from_slice(&iloc);
        let meta = bx(b"meta", &meta_body);
        let mdat = bx(b"mdat", &[1, 2, 3, 9, 9, 4, 5]);
        [ftyp, meta, mdat].concat()
    }

    #[test]
    fn gathers_extents() {
        assert_eq!(primary_item(&file(b"av02")), Ok(vec![1, 2, 3, 4, 5]));
    }

    #[test]
    fn refuses_av1() {
        assert_eq!(primary_item(&file(b"av01")), Err(Error::NotAv2));
    }

    #[test]
    fn every_truncation_is_an_error() {
        let f = file(b"av02");
        for n in 0..f.len() {
            assert!(primary_item(&f[..n]).is_err(), "prefix of {n} bytes");
        }
    }
}
