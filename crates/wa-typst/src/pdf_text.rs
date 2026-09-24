//! Test-only: read the text back out of an **uncompressed** (`pretty`)
//! typst-pdf file, so tests can prove an input string was laid out as glyphs
//! rather than trusting that "some bytes came out".
//!
//! typst-pdf (through krilla) writes text as 2-byte glyph ids in literal
//! strings between `BT` and `ET`, and one `ToUnicode` character map per font
//! that maps those ids to UTF-16BE. This decodes every text string with every
//! map and returns one text per map: a run set in font F reads correctly in
//! F's text and as replacement characters in the others. No positioning, no
//! reading order across columns: just enough to find a string.

use std::collections::HashMap;

/// Whether `needle` appears in the PDF's text, ignoring case and whitespace
/// (a wrapped line loses its break space; `upper()` changes case).
pub(crate) fn contains(pdf: &[u8], needle: &str) -> bool {
    let needle = normalize(needle);
    texts(pdf)
        .iter()
        .any(|text| normalize(text).contains(&needle))
}

/// The document text decoded with each font's character map, in content
/// order.
pub(crate) fn texts(pdf: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(pdf);
    // Only `bfchar` is decoded below; fail loudly if krilla ever switches to
    // ranges rather than silently finding nothing.
    assert!(
        !text.contains("beginbfrange"),
        "bfrange CMaps are not decoded"
    );
    let cmaps = cmaps(&text);
    assert!(
        !cmaps.is_empty(),
        "no ToUnicode CMap: is this a pretty PDF with text?"
    );
    let runs = text_strings(pdf);
    cmaps
        .iter()
        .map(|cmap| runs.iter().map(|run| decode(run, cmap)).collect())
        .collect()
}

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Every `begincmap … endcmap` section's `bfchar` entries.
fn cmaps(text: &str) -> Vec<HashMap<u16, String>> {
    text.split("begincmap")
        .skip(1)
        .map(|section| {
            let section = section.split("endcmap").next().unwrap_or_default();
            let mut map = HashMap::new();
            for block in section.split("beginbfchar").skip(1) {
                let block = block.split("endbfchar").next().unwrap_or_default();
                let hex: Vec<&str> = block
                    .split(['<', '>'])
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .collect();
                for pair in hex.chunks(2) {
                    if let [code, unicode] = pair {
                        let code = u16::from_str_radix(code, 16).expect("glyph id");
                        map.insert(code, utf16_hex(unicode));
                    }
                }
            }
            map
        })
        .collect()
}

fn utf16_hex(hex: &str) -> String {
    let units: Vec<u16> = hex
        .as_bytes()
        .chunks(4)
        .map(|unit| {
            u16::from_str_radix(std::str::from_utf8(unit).expect("ascii"), 16).expect("utf-16")
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Literal strings inside `BT … ET` text objects.
fn text_strings(pdf: &[u8]) -> Vec<Vec<u8>> {
    let mut runs = Vec::new();
    let mut in_text = false;
    let mut i = 0;
    while i < pdf.len() {
        if pdf[i] == b'(' {
            let (string, next) = literal(pdf, i + 1);
            if in_text {
                runs.push(string);
            }
            i = next;
        } else if is_operator(pdf, i, b"BT") {
            in_text = true;
            i += 2;
        } else if is_operator(pdf, i, b"ET") {
            in_text = false;
            i += 2;
        } else {
            i += 1;
        }
    }
    runs
}

fn is_operator(pdf: &[u8], i: usize, op: &[u8]) -> bool {
    pdf[i..].starts_with(op)
        && (i == 0 || pdf[i - 1].is_ascii_whitespace())
        && pdf.get(i + op.len()).is_none_or(u8::is_ascii_whitespace)
}

/// Parse a literal string body starting after its `(`; returns the bytes and
/// the index after the closing `)`.
fn literal(pdf: &[u8], mut i: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    let mut depth = 0usize;
    while let Some(&byte) = pdf.get(i) {
        i += 1;
        match byte {
            b'\\' => {
                let Some(&escaped) = pdf.get(i) else { break };
                i += 1;
                match escaped {
                    b'n' => out.push(b'\n'),
                    b'r' => out.push(b'\r'),
                    b't' => out.push(b'\t'),
                    b'b' => out.push(0x08),
                    b'f' => out.push(0x0C),
                    b'0'..=b'7' => {
                        let mut value = escaped - b'0';
                        for _ in 0..2 {
                            match pdf.get(i) {
                                Some(&digit @ b'0'..=b'7') => {
                                    value = value.wrapping_mul(8).wrapping_add(digit - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push(value);
                    }
                    b'\r' | b'\n' => {}
                    other => out.push(other),
                }
            }
            b'(' => {
                depth += 1;
                out.push(byte);
            }
            b')' if depth == 0 => return (out, i),
            b')' => {
                depth -= 1;
                out.push(byte);
            }
            _ => out.push(byte),
        }
    }
    (out, i)
}

fn decode(run: &[u8], cmap: &HashMap<u16, String>) -> String {
    run.chunks(2)
        .map(|pair| match pair {
            [hi, lo] => cmap
                .get(&u16::from_be_bytes([*hi, *lo]))
                .map_or("\u{FFFD}", String::as_str),
            _ => "\u{FFFD}",
        })
        .collect()
}
