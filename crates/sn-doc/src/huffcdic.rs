//! Huff/CDIC decompression — the `'DH'` compression word (17480) every
//! Kindle-produced `.azw`/`.azw3` carries, and the compression the
//! Mobipocket reader ([`crate::mobi`]) delegates to when record 0 names it.
//!
//! The structure the widely copied PalmDOC flag-byte description does not
//! prepare you for:
//!
//! 1. **The HUFF record holds two tables.** `off1` (u32 at HUFF+8) points at
//!    256 u32 "dict1" entries; `off2` (HUFF+12) at 64 u32 "dict2" entries.
//!    Each dict1 entry packs `codelen` (bits 0–4), `term` (bit 7) and
//!    `maxcode` (bits 8–31): the top byte of a 32-bit bitstream lookahead
//!    indexes dict1 directly, `codelen` bits are consumed, and for
//!    non-terminal entries dict2's mincode/maxcode ladder (entries consumed
//!    two at a time — a canonical Huffman walk) extends the code length
//!    until the code falls inside `[mincode[codelen], maxcode[codelen]]`.
//!    The phrase index is `maxcode − code`, right-shifted back to the
//!    codelen's scale.
//! 2. **The CDIC records hold the phrase dictionary.** After the 16-byte
//!    header sits a table of one u16 per phrase — cumulative byte offsets
//!    (from the table itself) to each entry's `[length][payload]` pair,
//!    the length's bit 15 marking "already expanded" — set on every plain
//!    text entry, clear on the ones that need the second pass. The iOS reference
//!    read the offsets as fixed-stride lengths and the payloads at
//!    `18 + 2·i` — every real book decodes as garbage that way; the real
//!    book this was validated on (a 7.6 MB LWW `Pocket Pediatrics`
//!    `.azw3`, 6052 dictionary entries) shows the offset table plainly
//!    (its first offset is `2·n`, the table's own end). A compressed
//!    entry recursively expands through the same bitstream — that book
//!    carries dozens that need the second pass.
//!
//! One instance per book: the dictionary is shared and lazily expanded
//! across all of that book's text records. Transcribed from the sibling
//! speechnotes-ios `HuffCdicReader.swift`, itself after KindleUnpack's
//! `HuffcdicReader` — the algorithm calibre runs.

/// Why a HUFF/CDIC table refused to load. The caller turns these into the
/// user-presentable "its compression tables are missing/corrupt" message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HuffError {
    BadHuffHeader,
    BadCdicHeader,
}

/// The tables and dictionary, built once per book.
pub struct Reader {
    /// dict1 entry: the code length this top-byte block starts, whether the
    /// entry is terminal (the index is final at this length), and the max
    /// code value the block covers, pre-shifted to the 32-bit scale.
    dict1: Vec<(u32, bool, i64)>,
    /// Canonical-code ladders for the non-terminal walk, indexed by codelen
    /// 1…32, values pre-shifted to the 32-bit lookahead's scale.
    mincode: Vec<i64>,
    maxcode: Vec<i64>,
    /// The CDIC phrase dictionary; compressed entries expand in place on
    /// first use.
    dictionary: Vec<(Vec<u8>, bool)>,
}

impl Reader {
    /// Builds the tables from one HUFF record and its CDIC dictionary
    /// records. Missing or corrupt tables are an error, not a fallback: the
    /// alternative is decoding the bitstream as noise.
    pub fn new(huff: &[u8], cdics: &[&[u8]]) -> Result<Self, HuffError> {
        if huff.len() < 16 || huff[..4] != *b"HUFF" {
            return Err(HuffError::BadHuffHeader);
        }
        let off1 = u32(huff, 8) as usize;
        let off2 = u32(huff, 12) as usize;
        if off1 < 24 || off2 < off1 || off2 + 4 * 64 > huff.len() {
            return Err(HuffError::BadHuffHeader);
        }

        let mut dict1 = Vec::with_capacity(256);
        for index in 0..256 {
            let value = u32(huff, off1 + 4 * index);
            let codelen = value & 0x1F;
            if codelen == 0 {
                return Err(HuffError::BadHuffHeader);
            }
            let term = value & 0x80 != 0;
            // ((maxcode + 1) << (32 - codelen)) - 1, on the 24-bit payload.
            // The parens are load-bearing: the shift applies to the
            // incremented value, and the `- 1` sits outside it — a form that
            // reads the other way produces every maxcode half-size.
            let maxcode = (((value >> 8) as i64 + 1) << (32 - codelen)) - 1;
            dict1.push((codelen, term, maxcode));
        }

        // dict2: 64 u32s consumed as (mincode, maxcode) pairs per codelen
        // 1…32. Both ladders carry a 0 sentinel at index 0 so the walk's
        // `codelen` can index them directly.
        let mut mincode = vec![0i64];
        let mut maxcode = vec![0i64];
        for codelen in 1..=32usize {
            if off2 + 8 * (codelen - 1) + 8 > huff.len() {
                return Err(HuffError::BadHuffHeader);
            }
            let min = u32(huff, off2 + 8 * (codelen - 1)) as i64;
            let max = u32(huff, off2 + 8 * (codelen - 1) + 4) as i64;
            mincode.push(min << (32 - codelen));
            maxcode.push(((max + 1) << (32 - codelen)) - 1);
        }

        let mut dictionary: Vec<(Vec<u8>, bool)> = Vec::new();
        let mut remaining_phrases: i64 = -1;
        for cdic in cdics {
            if cdic.len() < 16 || cdic[..4] != *b"CDIC" {
                return Err(HuffError::BadCdicHeader);
            }
            let phrases = u32(cdic, 8) as i64;
            let bits = (u32(cdic, 12) & 0xF) as i64;
            if remaining_phrases < 0 {
                remaining_phrases = phrases;
            }
            // n = min(1 << bits, phrases - alreadyCollected) — a multi-CDIC
            // dictionary fills each record to the smaller of its index
            // capacity and what is left of the book's phrase count.
            let already = dictionary.len() as i64;
            let declared = (1i64 << bits).min((phrases - already).max(0));
            let count = declared.min(remaining_phrases) as usize;
            // The offset table: one u16 per entry, cumulative from the
            // table's own start (so the first offset is 2·count — the
            // table's end). Each entry is `[length u16][payload]` at its
            // offset; bit 15 of the length marks it still compressed.
            let table_end = 16 + 2 * count;
            if table_end > cdic.len() {
                return Err(HuffError::BadCdicHeader);
            }
            for index in 0..count {
                let entry_at = 16 + u16(cdic, 16 + 2 * index) as usize;
                if entry_at + 2 > cdic.len() {
                    return Err(HuffError::BadCdicHeader);
                }
                let header = u16(cdic, entry_at);
                let length = (header & 0x7FFF) as usize;
                let payload_at = entry_at + 2;
                if payload_at + length > cdic.len() {
                    return Err(HuffError::BadCdicHeader);
                }
                // Bit 15 SET is a plain entry; clear means the payload is
                // itself a bitstream and expands on first use. (The iOS
                // reference read the bit inverted — every real book decodes
                // as garbage that way; the entries above settle it: `fr`
                // and `on` carry the bit, the binary fragment doesn't.)
                dictionary.push((
                    cdic[payload_at..payload_at + length].to_vec(),
                    header & 0x8000 != 0,
                ));
            }
            remaining_phrases -= count as i64;
        }
        if dictionary.is_empty() {
            return Err(HuffError::BadCdicHeader);
        }

        Ok(Self {
            dict1,
            mincode,
            maxcode,
            dictionary,
        })
    }

    /// Decompresses one record payload (its trailing-entry table already
    /// stripped by the caller). Reads the bitstream through a 64-bit window:
    /// `code` is the 32 bits starting at bit `n`; consuming `codelen` bits
    /// decrements `n`, and when `n` runs dry the window slides four bytes —
    /// padded with eight zero bytes so the final lookahead never reads past
    /// the end. The loop ends on `bits_left < 0`, not on the padding.
    pub fn unpack(&mut self, payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(payload.len() + 8);
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&[0u8; 8]);
        let mut bits_left = (payload.len() * 8) as i64;
        let mut pos = 0usize;
        let mut window = load64(&bytes, 0);
        let mut n: i64 = 32;

        let mut out: Vec<u8> = Vec::with_capacity(4096);

        loop {
            if n <= 0 {
                pos += 4;
                if pos + 8 > bytes.len() {
                    break;
                }
                window = load64(&bytes, pos);
                n += 32;
            }
            // code = (window >> n) & 0xFFFFFFFF — the 32-bit lookahead.
            let code = ((window >> n) & 0xFFFF_FFFF) as i64;
            let (base_codelen, term, table_max) = self.dict1[(code >> 24) as usize];
            let mut codelen = base_codelen as usize;
            let mut maxcode = table_max;
            if !term {
                // Canonical walk: extend the code length until the code
                // falls inside [mincode[codelen], maxcode[codelen]].
                while codelen < self.mincode.len() && code < self.mincode[codelen] {
                    codelen += 1;
                }
                if codelen >= self.maxcode.len() {
                    break;
                }
                maxcode = self.maxcode[codelen];
            }
            n -= codelen as i64;
            bits_left -= codelen as i64;
            if bits_left < 0 {
                break;
            }
            let reference = (maxcode - code) >> (32 - codelen);
            if reference < 0 || reference as usize >= self.dictionary.len() {
                break;
            }
            let reference = reference as usize;
            if !self.dictionary[reference].1 {
                let raw = std::mem::take(&mut self.dictionary[reference].0);
                self.dictionary[reference].1 = true;
                let expanded = self.unpack(&raw);
                self.dictionary[reference].0 = expanded;
            }
            out.extend_from_slice(&self.dictionary[reference].0);
        }
        out
    }
}

fn load64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = 0u64;
    for index in 0..8 {
        value = (value << 8) | bytes[offset + index] as u64;
    }
    value
}

fn u16(data: &[u8], offset: usize) -> u16 {
    if offset + 2 > data.len() {
        return 0;
    }
    ((data[offset] as u16) << 8) | data[offset + 1] as u16
}

fn u32(data: &[u8], offset: usize) -> u32 {
    if offset + 4 > data.len() {
        return 0;
    }
    ((data[offset] as u32) << 24)
        | ((data[offset + 1] as u32) << 16)
        | ((data[offset + 2] as u32) << 8)
        | data[offset + 3] as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal but REAL Huff/CDIC table pair: a canonical Huffman code
    /// where every top byte is a terminal 8-bit entry, and a CDIC dictionary
    /// of 256 identical phrases. Code 0x00 decodes through dict1[0] to
    /// dictionary index (maxcode − code) >> 24 = 255 — the whole path:
    /// HUFF table parse, CDIC dictionary parse, terminal lookup, output.
    fn terminal_fixture() -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut huff = Vec::new();
        huff.extend_from_slice(b"HUFF");
        huff.extend_from_slice(&0x18u32.to_be_bytes()); // header length
        huff.extend_from_slice(&24u32.to_be_bytes()); // off1
        huff.extend_from_slice(&(24u32 + 4 * 256).to_be_bytes()); // off2
        huff.extend_from_slice(&[0u8; 8]);
        for _ in 0..256 {
            // codelen 8 (bits 0–4), terminal (bit 7), maxcode byte 0xFF:
            // value 0xFF88 → maxcode = ((0xFF + 1) << 24) − 1 = 0xFFFFFFFF,
            // so code 0x00 resolves to dictionary index 255.
            huff.extend_from_slice(&0xFF88u32.to_be_bytes());
        }
        // dict2: 64 zero u32s — terminal entries never walk it, zeros parse.
        huff.extend_from_slice(&[0u8; 4 * 64]);

        // CDIC: 256 phrases (index bits 8 → 1<<8 = 256), all "Hi ".
        let entries: Vec<(Vec<u8>, bool)> = (0..256).map(|_| (b"Hi ".to_vec(), false)).collect();
        (huff, vec![build_cdic(&entries)])
    }

    /// A CDIC record built the way real writers lay it out: the 16-byte
    /// header, the cumulative offset table, then `[length][payload]`
    /// entries; a `true` flag marks an entry still compressed.
    fn build_cdic(entries: &[(Vec<u8>, bool)]) -> Vec<u8> {
        let mut cdic = Vec::new();
        cdic.extend_from_slice(b"CDIC");
        cdic.extend_from_slice(&0x10u32.to_be_bytes()); // header length
        cdic.extend_from_slice(&(entries.len() as u32).to_be_bytes());
        cdic.extend_from_slice(&8u32.to_be_bytes()); // index bits
        let mut offset = 2 * entries.len();
        for (payload, _) in entries {
            cdic.extend_from_slice(&(offset as u16).to_be_bytes());
            offset += 2 + payload.len();
        }
        for (payload, compressed) in entries {
            // Bit 15 SET = plain; clear = the payload is itself a bitstream.
            let header =
                payload.len() as u16 | if *compressed { 0 } else { 0x8000 };
            cdic.extend_from_slice(&header.to_be_bytes());
            cdic.extend_from_slice(payload);
        }
        cdic
    }

    #[test]
    fn a_terminal_literal_book_decodes() {
        let (huff, cdics) = terminal_fixture();
        let cdic_refs: Vec<&[u8]> = cdics.iter().map(|c| c.as_slice()).collect();
        let mut reader = Reader::new(&huff, &cdic_refs).unwrap();
        // 3 codes → "Hi Hi Hi "
        let out = reader.unpack(&[0x00, 0x00, 0x00]);
        assert_eq!(out, b"Hi Hi Hi ");
    }

    #[test]
    fn a_compressed_dictionary_entry_expands_on_first_use() {
        // The same fixture, but phrase 254 is "still compressed" (bit 15
        // set): its payload is itself a bitstream of three 0x00 codes,
        // which resolve through the dictionary — to phrase 255. Real
        // store books carry dozens of entries like this (the validation
        // book, a 6052-entry LWW dictionary, needed the second pass
        // often).
        let (huff, _) = terminal_fixture();
        let mut entries: Vec<(Vec<u8>, bool)> =
            (0..256).map(|_| (b"Hi ".to_vec(), false)).collect();
        entries[254] = (vec![0x00, 0x00, 0x00], true);
        let cdic = build_cdic(&entries);

        let cdic_refs: Vec<&[u8]> = vec![cdic.as_slice()];
        let mut reader = Reader::new(&huff, &cdic_refs).unwrap();
        // Code 0x01000000 (top byte 0x01) → phrase 254 → its payload
        // decodes to 3 × phrase; the three 0x00 codes after it → phrase
        // 255 each.
        let out = reader.unpack(&[0x01, 0x00, 0x00, 0x00]);
        // The expanded entry (3 × phrase) plus the three whole 0x00 codes
        // that fit the payload's 32 bits — the fourth lands on the final
        // bit and the padding beyond it decodes nothing.
        assert_eq!(out, b"Hi Hi Hi Hi Hi Hi ");
        // The expansion is cached: a second pass reuses it.
        let again = reader.unpack(&[0x01, 0x00, 0x00, 0x00]);
        assert_eq!(again, b"Hi Hi Hi Hi Hi Hi ");
    }

    #[test]
    fn corrupt_tables_are_errors_not_noise() {
        let (huff, cdics) = terminal_fixture();
        let cdic_refs: Vec<&[u8]> = cdics.iter().map(|c| c.as_slice()).collect();
        let bad_huff = |result| matches!(result, Err(HuffError::BadHuffHeader));
        let bad_cdic = |result| matches!(result, Err(HuffError::BadCdicHeader));
        // Wrong magic, truncated tables, a zero codelen — all refused.
        assert!(bad_huff(Reader::new(b"nope", &cdic_refs)));
        assert!(bad_huff(Reader::new(&huff[..10], &cdic_refs)));
        let mut zero_codelen = huff.clone();
        zero_codelen[24..28].copy_from_slice(&0xFF00u32.to_be_bytes()); // codelen 0
        assert!(bad_huff(Reader::new(&zero_codelen, &cdic_refs)));
        assert!(bad_cdic(Reader::new(&huff, &[])));
        assert!(bad_cdic(Reader::new(&huff, &[&b"xxxx"[..]])));
    }

    #[test]
    fn a_truncated_bitstream_stops_cleanly() {
        let (huff, cdics) = terminal_fixture();
        let cdic_refs: Vec<&[u8]> = cdics.iter().map(|c| c.as_slice()).collect();
        let mut reader = Reader::new(&huff, &cdic_refs).unwrap();
        // A payload shorter than the bits it claims still decodes the whole
        // codes it has and stops — the padding keeps the lookahead in bounds.
        let out = reader.unpack(&[0x00]);
        assert_eq!(out, b"Hi ");
        assert!(reader.unpack(&[]).is_empty());
    }
}
