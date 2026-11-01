//! An MP4 (M4A, 3GP, MOV) whose index comes after its media data, put
//! index first, as ffmpeg's own `-movflags +faststart` does, so that it
//! can be read from a pipe.
//!
//! ffmpeg reads the input from stdin, which cannot seek. An MP4's index
//! (the `moov` box) says where each sample is; when it follows the media
//! data (`mdat`), as many recorders write it, ffmpeg reaches it only past
//! the samples, and cannot go back to them once they are out of its
//! buffer: the input would decode to nothing, or almost. Moving `moov`
//! before the first `mdat` moves the media data by the index's length, so
//! every chunk offset in it (`stco`, `co64`) that points into the data
//! moved grows by as much.
//!
//! Only what that needs is read, every read is bounds-checked, and any
//! input that is not such an MP4, or that this does not understand (a
//! compressed index, a fragmented file, a box past the end), is left as it
//! is, for ffmpeg to read or refuse.

/// The most boxes walked in one container: an MP4 has a handful.
const MAX_BOXES: usize = 1024;

/// The containers from `moov` down to the chunk offset tables, one per
/// level: no other box is entered, so a crafted nesting cannot recurse
/// deeper than this.
const PATH: [[u8; 4]; 4] = [*b"trak", *b"mdia", *b"minf", *b"stbl"];

/// A box: where it starts, its header's length and its whole length.
#[derive(Debug, Clone, Copy)]
struct Mp4Box {
    start: usize,
    header: usize,
    len: usize,
    kind: [u8; 4],
}

fn read_u32(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn read_u64(data: &[u8], at: usize) -> Option<u64> {
    let bytes = data.get(at..at.checked_add(8)?)?;
    Some(u64::from_be_bytes(bytes.try_into().ok()?))
}

/// The box at `at`, which must end by `end`; `None` when it does not, or
/// its header is not one.
fn box_at(data: &[u8], at: usize, end: usize) -> Option<Mp4Box> {
    let size = read_u32(data, at)?;
    let kind: [u8; 4] = data
        .get(at.checked_add(4)?..at.checked_add(8)?)?
        .try_into()
        .ok()?;
    let (header, len) = match size {
        // To the end of its container.
        0 => (8, end.checked_sub(at)?),
        // A 64-bit size follows the type.
        1 => (
            16,
            usize::try_from(read_u64(data, at.checked_add(8)?)?).ok()?,
        ),
        n => (8, usize::try_from(n).ok()?),
    };
    if len < header || at.checked_add(len)? > end {
        return None;
    }
    Some(Mp4Box {
        start: at,
        header,
        len,
        kind,
    })
}

/// The boxes of `data[from..end]`, in order; `None` when they do not
/// tile it exactly or there are more than [`MAX_BOXES`].
fn boxes(data: &[u8], from: usize, end: usize) -> Option<Vec<Mp4Box>> {
    let mut out = Vec::new();
    let mut at = from;
    while at < end {
        if out.len() == MAX_BOXES {
            return None;
        }
        let b = box_at(data, at, end)?;
        at = b.start.checked_add(b.len)?;
        out.push(b);
    }
    Some(out)
}

/// `data` with its index moved before its media data, or `None` to leave
/// it as it is: not an MP4, already index first, fragmented, or anything
/// this does not understand.
pub(super) fn index_first(data: &[u8]) -> Option<Vec<u8>> {
    let top = boxes(data, 0, data.len())?;
    let mdat = top.iter().position(|b| &b.kind == b"mdat")?;
    let moov = top.iter().position(|b| &b.kind == b"moov")?;
    let indexes = top.iter().filter(|b| &b.kind == b"moov").count();
    let fragmented = top.iter().any(|b| &b.kind == b"moof")
        || boxes(
            data,
            top.get(moov)?.start.checked_add(top.get(moov)?.header)?,
            top.get(moov)?.start.checked_add(top.get(moov)?.len)?,
        )?
        .iter()
        .any(|b| &b.kind == b"mvex");
    if moov < mdat || indexes != 1 || fragmented {
        return None;
    }
    let (first, index) = (top.get(mdat)?.start, *top.get(moov)?);
    // A size of 0 means "to the end of the file", which `moov` no longer
    // is once moved.
    if read_u32(data, index.start)? == 0 {
        return None;
    }
    let index_end = index.start.checked_add(index.len)?;
    let mut moved = data.get(index.start..index_end)?.to_vec();
    let shift = u64::try_from(index.len).ok()?;
    let (low, high) = (u64::try_from(first).ok()?, u64::try_from(index.start).ok()?);
    let mut tables = 0;
    shift_offsets(
        &mut moved,
        index.header,
        index.len,
        0,
        &mut tables,
        &|offset| {
            if (low..high).contains(&offset) {
                offset.checked_add(shift)
            } else {
                Some(offset)
            }
        },
    )?;
    // A chunk table this walk did not reach (under `dinf`, `edts`, or a
    // second one in a `stbl`) may be the one ffmpeg reads, and it would
    // keep its old offsets: leave such a file as it is. A false match (the
    // four bytes inside opaque metadata) only costs the move.
    let named = moved
        .windows(4)
        .filter(|w| *w == b"stco" || *w == b"co64")
        .count();
    if named != tables {
        return None;
    }
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(data.get(..first)?);
    out.extend_from_slice(&moved);
    out.extend_from_slice(data.get(first..index.start)?);
    out.extend_from_slice(data.get(index_end..)?);
    Some(out)
}

/// Apply `shift` to every chunk offset under the container `buf[from..end]`,
/// at `depth` on [`PATH`] (0: the children of `moov`); `None` when a box
/// does not parse, the index is compressed (`cmov`), or an offset would not
/// fit its field.
fn shift_offsets(
    buf: &mut [u8],
    from: usize,
    end: usize,
    depth: usize,
    tables: &mut usize,
    shift: &dyn Fn(u64) -> Option<u64>,
) -> Option<()> {
    for b in boxes(buf, from, end)? {
        let body = b.start.checked_add(b.header)?;
        let b_end = b.start.checked_add(b.len)?;
        match (&b.kind, PATH.get(depth)) {
            (b"cmov", _) => return None,
            (kind, Some(next)) if kind == next => {
                shift_offsets(buf, body, b_end, depth + 1, tables, shift)?;
            }
            (b"stco" | b"co64", None) => {
                *tables = tables.checked_add(1)?;
                let wide = &b.kind == b"co64";
                let width = if wide { 8 } else { 4 };
                // Version and flags, then the entry count.
                let count = usize::try_from(read_u32(buf, body.checked_add(4)?)?).ok()?;
                let entries = body.checked_add(8)?;
                if count.checked_mul(width)?.checked_add(entries)? > b_end {
                    return None;
                }
                for k in 0..count {
                    let at = entries.checked_add(k.checked_mul(width)?)?;
                    if wide {
                        let shifted = shift(read_u64(buf, at)?)?;
                        buf.get_mut(at..at.checked_add(8)?)?
                            .copy_from_slice(&shifted.to_be_bytes());
                    } else {
                        let shifted = u32::try_from(shift(u64::from(read_u32(buf, at)?))?).ok()?;
                        buf.get_mut(at..at.checked_add(4)?)?
                            .copy_from_slice(&shifted.to_be_bytes());
                    }
                }
            }
            _ => {}
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp4_box(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(8 + body.len())
            .unwrap()
            .to_be_bytes()
            .to_vec();
        out.extend_from_slice(&kind);
        out.extend_from_slice(body);
        out
    }

    fn table(kind: [u8; 4], offsets: &[u64]) -> Vec<u8> {
        let mut body = vec![0; 4];
        body.extend_from_slice(&u32::try_from(offsets.len()).unwrap().to_be_bytes());
        for &o in offsets {
            if &kind == b"co64" {
                body.extend_from_slice(&o.to_be_bytes());
            } else {
                body.extend_from_slice(&u32::try_from(o).unwrap().to_be_bytes());
            }
        }
        mp4_box(kind, &body)
    }

    /// `ftyp`, `mdat` (two chunks: "AAAA" and "BBBB"), then `moov` with
    /// one track whose chunk table (`kind`) points at them.
    fn index_last(kind: [u8; 4]) -> Vec<u8> {
        let ftyp = mp4_box(*b"ftyp", b"M4A \0\0\0\0");
        let mdat = mp4_box(*b"mdat", b"AAAABBBB");
        let a = u64::try_from(ftyp.len() + 8).unwrap();
        let stbl = mp4_box(*b"stbl", &table(kind, &[a, a + 4]));
        let minf = mp4_box(*b"minf", &stbl);
        let mdia = mp4_box(*b"mdia", &minf);
        let trak = mp4_box(*b"trak", &[mp4_box(*b"tkhd", &[0; 4]), mdia].concat());
        let moov = mp4_box(*b"moov", &[mp4_box(*b"mvhd", &[0; 4]), trak].concat());
        [ftyp, mdat, moov].concat()
    }

    /// The chunk offsets of the first table found, and the data at each.
    fn chunks(file: &[u8]) -> Vec<(u64, Vec<u8>)> {
        let wide = file.windows(4).position(|w| w == b"co64");
        let at = wide
            .or_else(|| file.windows(4).position(|w| w == b"stco"))
            .unwrap()
            + 4;
        let count = usize::try_from(read_u32(file, at + 4).unwrap()).unwrap();
        (0..count)
            .map(|k| {
                let o = if wide.is_some() {
                    read_u64(file, at + 8 + 8 * k).unwrap()
                } else {
                    u64::from(read_u32(file, at + 8 + 4 * k).unwrap())
                };
                let s = usize::try_from(o).unwrap();
                (o, file[s..s + 4].to_vec())
            })
            .collect()
    }

    #[test]
    fn the_index_moves_first_and_its_offsets_follow_the_data() {
        for kind in [b"stco", b"co64"] {
            let file = index_last(*kind);
            assert_eq!(
                chunks(&file)
                    .into_iter()
                    .map(|(_, d)| d)
                    .collect::<Vec<_>>(),
                [b"AAAA".to_vec(), b"BBBB".to_vec()]
            );
            let out = index_first(&file).unwrap();
            assert_eq!(out.len(), file.len());
            let top: Vec<[u8; 4]> = boxes(&out, 0, out.len())
                .unwrap()
                .iter()
                .map(|b| b.kind)
                .collect();
            assert_eq!(top, [*b"ftyp", *b"moov", *b"mdat"]);
            // Each offset still points at its chunk.
            assert_eq!(
                chunks(&out).into_iter().map(|(_, d)| d).collect::<Vec<_>>(),
                [b"AAAA".to_vec(), b"BBBB".to_vec()],
                "{}",
                String::from_utf8_lossy(kind)
            );
        }
    }

    #[test]
    fn anything_else_is_left_as_it_is() {
        let file = index_last(*b"stco");
        let boxes_of = |f: &[u8]| boxes(f, 0, f.len()).unwrap();
        // Already index first.
        let first = index_first(&file).unwrap();
        assert_eq!(index_first(&first), None);
        // Not an MP4.
        assert_eq!(index_first(b"RIFF\x24\0\0\0WAVEfmt "), None);
        assert_eq!(index_first(b""), None);
        // Truncated: the last box runs past the end.
        assert_eq!(index_first(&file[..file.len() - 1]), None);
        // Fragmented.
        let fragmented = [file.clone(), mp4_box(*b"moof", &[])].concat();
        assert_eq!(index_first(&fragmented), None);
        // A compressed index.
        let ftyp_mdat = boxes_of(&file)[2].start;
        let cmov = [
            &file[..ftyp_mdat],
            &mp4_box(*b"moov", &mp4_box(*b"cmov", &[0; 8])),
        ]
        .concat();
        assert_eq!(index_first(&cmov), None);
        // A table claiming more entries than it holds.
        let mut lying = file.clone();
        let at = lying.windows(4).position(|w| w == b"stco").unwrap() + 8;
        lying[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(index_first(&lying), None);
        // A box declaring a 64-bit size past the end.
        let mut huge = file;
        huge[..4].copy_from_slice(&1u32.to_be_bytes());
        assert_eq!(index_first(&huge), None);
    }

    /// A file whose index holds `extra` under `minf`, next to the `stbl`.
    fn index_last_with(extra: &[u8]) -> Vec<u8> {
        let ftyp = mp4_box(*b"ftyp", b"M4A \0\0\0\0");
        let mdat = mp4_box(*b"mdat", b"AAAABBBB");
        let a = u64::try_from(ftyp.len() + 8).unwrap();
        let stbl = mp4_box(*b"stbl", &table(*b"stco", &[a, a + 4]));
        let minf = mp4_box(*b"minf", &[extra, &stbl].concat());
        let mdia = mp4_box(*b"mdia", &minf);
        let trak = mp4_box(*b"trak", &[mp4_box(*b"tkhd", &[0; 4]), mdia].concat());
        let moov = mp4_box(*b"moov", &[mp4_box(*b"mvhd", &[0; 4]), trak].concat());
        [ftyp, mdat, moov].concat()
    }

    /// A chunk table off the walked path (here under `dinf`, which ffmpeg
    /// reads first) would keep its old offsets: such a file is left as it
    /// is rather than rewritten into one ffmpeg decodes to near silence.
    #[test]
    fn a_chunk_table_off_the_walked_path_leaves_the_file_as_it_is() {
        let a = u64::try_from(mp4_box(*b"ftyp", b"M4A \0\0\0\0").len() + 8).unwrap();
        let dinf = mp4_box(*b"dinf", &table(*b"stco", &[a]));
        assert_eq!(index_first(&index_last_with(&dinf)), None);
        // The same file without the stray table is rewritten.
        assert!(index_first(&index_last_with(&[])).is_some());
    }

    /// An index with `mvex` describes a fragmented file, even before its
    /// first `moof`: left as it is.
    #[test]
    fn an_index_with_mvex_is_left_as_it_is() {
        let file = index_last(*b"stco");
        let top = boxes(&file, 0, file.len()).unwrap();
        let moov = top[2];
        let mut body = file[moov.start + moov.header..moov.start + moov.len].to_vec();
        body.extend_from_slice(&mp4_box(*b"mvex", &[]));
        let fragmented = [&file[..moov.start], &mp4_box(*b"moov", &body)].concat();
        assert_eq!(index_first(&fragmented), None);
    }

    /// A `trak` in a `trak` in a `trak`, a hundred thousand deep, is not
    /// entered past the first: no recursion follows the input's nesting.
    #[test]
    fn a_deep_nesting_is_not_followed() {
        const DEPTH: usize = 100_000;
        let inner = table(*b"stco", &[0]);
        let mut nested = Vec::with_capacity(8 * DEPTH + inner.len());
        for level in 0..DEPTH {
            let size = 8 * (DEPTH - level) + inner.len();
            nested.extend_from_slice(&u32::try_from(size).unwrap().to_be_bytes());
            nested.extend_from_slice(b"trak");
        }
        nested.extend_from_slice(&inner);
        let ftyp = mp4_box(*b"ftyp", b"M4A \0\0\0\0");
        let file = [ftyp, mp4_box(*b"mdat", b"AAAA"), mp4_box(*b"moov", &nested)].concat();
        // Returning at all is the point (no stack overflow); the table
        // at the bottom is off the walked path, so the file is left as it
        // is.
        assert_eq!(index_first(&file), None);
        // And a `moov` sized "to the end of the file" is not moved.
        let mut to_end = file.clone();
        let at = to_end.windows(4).position(|w| w == b"moov").unwrap() - 4;
        to_end[at..at + 4].copy_from_slice(&0u32.to_be_bytes());
        assert_eq!(index_first(&to_end), None);
    }

    /// A table counting one entry more than it holds, followed by another
    /// box: the "entry" past its end is the next box's header, which is
    /// never rewritten.
    #[test]
    fn a_table_never_writes_past_its_box() {
        let mut stco = table(*b"stco", &[100, 200]);
        stco[12..16].copy_from_slice(&3u32.to_be_bytes());
        let mut buf = [stco, mp4_box(*b"stsz", &[0; 8])].concat();
        let before = buf.clone();
        let len = buf.len();
        assert_eq!(
            shift_offsets(&mut buf, 0, len, PATH.len(), &mut 0, &|o| o.checked_add(1)),
            None
        );
        assert_eq!(buf[24..], before[24..], "the next box was rewritten");
    }

    #[test]
    fn a_shift_that_overflows_a_32_bit_offset_is_refused() {
        let mut stco = table(*b"stco", &[u64::from(u32::MAX)]);
        let len = stco.len();
        assert_eq!(
            shift_offsets(&mut stco, 0, len, PATH.len(), &mut 0, &|o| o.checked_add(1)),
            None
        );
        let mut co64 = table(*b"co64", &[u64::from(u32::MAX)]);
        let len = co64.len();
        assert_eq!(
            shift_offsets(&mut co64, 0, len, PATH.len(), &mut 0, &|o| o.checked_add(1)),
            Some(())
        );
        assert_eq!(read_u64(&co64, 16), Some(u64::from(u32::MAX) + 1));
    }
}
