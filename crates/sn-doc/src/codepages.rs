//! The single-byte codepages the book formats arrive in.
//!
//! A `.fb2` declares its encoding in its XML declaration; an `.rtf` names
//! it in `\ansicpg`; a `.mobi` states it in its header. All of them are
//! one-byte encodings whose high half is a table — so the whole battery is
//! a static table and a lookup, not a dependency.
//!
//! The tables are the codepages' own mappings, verified against the
//! reference codec tables rather than written from memory: a Russian
//! `ё` that decodes to `ц` is a book that reads wrong from the first
//! paragraph, and no test would notice unless the test knew the right
//! answer.
//!
//! The low half (0x00–0x7F) is ASCII in every one of them, so only the
//! high half is stored. A byte with no meaning in its codepage keeps its
//! Latin-1 glyph rather than becoming a replacement character — the
//! reader shows what the file carried.

/// One codepage's high half: 128 characters, 0x80..=0xFF.
type HighHalf = [&'static str; 4];

/// The high half of Central European (Polish, Czech, Hungarian).
const CP1250: HighHalf = [
    "\u{20ac}\u{81}\u{201a}\u{83}\u{201e}\u{2026}\u{2020}\u{2021}\u{88}\u{2030}\u{160}\u{2039}\u{15a}\u{164}\u{17d}\u{179}\u{90}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{98}\u{2122}\u{161}\u{203a}\u{15b}\u{165}\u{17e}\u{17a}",
    "\u{a0}\u{2c7}\u{2d8}\u{141}\u{a4}\u{104}\u{a6}\u{a7}\u{a8}\u{a9}\u{15e}\u{ab}\u{ac}\u{ad}\u{ae}\u{17b}\u{b0}\u{b1}\u{2db}\u{142}\u{b4}\u{b5}\u{b6}\u{b7}\u{b8}\u{105}\u{15f}\u{bb}\u{13d}\u{2dd}\u{13e}\u{17c}",
    "\u{154}\u{c1}\u{c2}\u{102}\u{c4}\u{139}\u{106}\u{c7}\u{10c}\u{c9}\u{118}\u{cb}\u{11a}\u{cd}\u{ce}\u{10e}\u{110}\u{143}\u{147}\u{d3}\u{d4}\u{150}\u{d6}\u{d7}\u{158}\u{16e}\u{da}\u{170}\u{dc}\u{dd}\u{162}\u{df}",
    "\u{155}\u{e1}\u{e2}\u{103}\u{e4}\u{13a}\u{107}\u{e7}\u{10d}\u{e9}\u{119}\u{eb}\u{11b}\u{ed}\u{ee}\u{10f}\u{111}\u{144}\u{148}\u{f3}\u{f4}\u{151}\u{f6}\u{f7}\u{159}\u{16f}\u{fa}\u{171}\u{fc}\u{fd}\u{163}\u{2d9}",
];

/// The high half of Cyrillic (the Russian FictionBook corpus).
const CP1251: HighHalf = [
    "\u{402}\u{403}\u{201a}\u{453}\u{201e}\u{2026}\u{2020}\u{2021}\u{20ac}\u{2030}\u{409}\u{2039}\u{40a}\u{40c}\u{40b}\u{40f}\u{452}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{98}\u{2122}\u{459}\u{203a}\u{45a}\u{45c}\u{45b}\u{45f}",
    "\u{a0}\u{40e}\u{45e}\u{408}\u{a4}\u{490}\u{a6}\u{a7}\u{401}\u{a9}\u{404}\u{ab}\u{ac}\u{ad}\u{ae}\u{407}\u{b0}\u{b1}\u{406}\u{456}\u{491}\u{b5}\u{b6}\u{b7}\u{451}\u{2116}\u{454}\u{bb}\u{458}\u{405}\u{455}\u{457}",
    "\u{410}\u{411}\u{412}\u{413}\u{414}\u{415}\u{416}\u{417}\u{418}\u{419}\u{41a}\u{41b}\u{41c}\u{41d}\u{41e}\u{41f}\u{420}\u{421}\u{422}\u{423}\u{424}\u{425}\u{426}\u{427}\u{428}\u{429}\u{42a}\u{42b}\u{42c}\u{42d}\u{42e}\u{42f}",
    "\u{430}\u{431}\u{432}\u{433}\u{434}\u{435}\u{436}\u{437}\u{438}\u{439}\u{43a}\u{43b}\u{43c}\u{43d}\u{43e}\u{43f}\u{440}\u{441}\u{442}\u{443}\u{444}\u{445}\u{446}\u{447}\u{448}\u{449}\u{44a}\u{44b}\u{44c}\u{44d}\u{44e}\u{44f}",
];

/// The high half of Western European (the HTML/RTF default).
const CP1252: HighHalf = [
    "\u{20ac}\u{81}\u{201a}\u{192}\u{201e}\u{2026}\u{2020}\u{2021}\u{2c6}\u{2030}\u{160}\u{2039}\u{152}\u{8d}\u{17d}\u{8f}\u{90}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{2dc}\u{2122}\u{161}\u{203a}\u{153}\u{9d}\u{17e}\u{178}",
    "\u{a0}\u{a1}\u{a2}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}\u{a8}\u{a9}\u{aa}\u{ab}\u{ac}\u{ad}\u{ae}\u{af}\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}\u{b8}\u{b9}\u{ba}\u{bb}\u{bc}\u{bd}\u{be}\u{bf}",
    "\u{c0}\u{c1}\u{c2}\u{c3}\u{c4}\u{c5}\u{c6}\u{c7}\u{c8}\u{c9}\u{ca}\u{cb}\u{cc}\u{cd}\u{ce}\u{cf}\u{d0}\u{d1}\u{d2}\u{d3}\u{d4}\u{d5}\u{d6}\u{d7}\u{d8}\u{d9}\u{da}\u{db}\u{dc}\u{dd}\u{de}\u{df}",
    "\u{e0}\u{e1}\u{e2}\u{e3}\u{e4}\u{e5}\u{e6}\u{e7}\u{e8}\u{e9}\u{ea}\u{eb}\u{ec}\u{ed}\u{ee}\u{ef}\u{f0}\u{f1}\u{f2}\u{f3}\u{f4}\u{f5}\u{f6}\u{f7}\u{f8}\u{f9}\u{fa}\u{fb}\u{fc}\u{fd}\u{fe}\u{ff}",
];

/// The high half of Greek.
const CP1253: HighHalf = [
    "\u{20ac}\u{81}\u{201a}\u{192}\u{201e}\u{2026}\u{2020}\u{2021}\u{88}\u{2030}\u{8a}\u{2039}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{98}\u{2122}\u{9a}\u{203a}\u{9c}\u{9d}\u{9e}\u{9f}",
    "\u{a0}\u{385}\u{386}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}\u{a8}\u{a9}\u{aa}\u{ab}\u{ac}\u{ad}\u{ae}\u{2015}\u{b0}\u{b1}\u{b2}\u{b3}\u{384}\u{b5}\u{b6}\u{b7}\u{388}\u{389}\u{38a}\u{bb}\u{38c}\u{bd}\u{38e}\u{38f}",
    "\u{390}\u{391}\u{392}\u{393}\u{394}\u{395}\u{396}\u{397}\u{398}\u{399}\u{39a}\u{39b}\u{39c}\u{39d}\u{39e}\u{39f}\u{3a0}\u{3a1}\u{d2}\u{3a3}\u{3a4}\u{3a5}\u{3a6}\u{3a7}\u{3a8}\u{3a9}\u{3aa}\u{3ab}\u{3ac}\u{3ad}\u{3ae}\u{3af}",
    "\u{3b0}\u{3b1}\u{3b2}\u{3b3}\u{3b4}\u{3b5}\u{3b6}\u{3b7}\u{3b8}\u{3b9}\u{3ba}\u{3bb}\u{3bc}\u{3bd}\u{3be}\u{3bf}\u{3c0}\u{3c1}\u{3c2}\u{3c3}\u{3c4}\u{3c5}\u{3c6}\u{3c7}\u{3c8}\u{3c9}\u{3ca}\u{3cb}\u{3cc}\u{3cd}\u{3ce}\u{ff}",
];

/// The high half of Turkish.
const CP1254: HighHalf = [
    "\u{20ac}\u{81}\u{201a}\u{192}\u{201e}\u{2026}\u{2020}\u{2021}\u{2c6}\u{2030}\u{160}\u{2039}\u{152}\u{8d}\u{8e}\u{8f}\u{90}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{2dc}\u{2122}\u{161}\u{203a}\u{153}\u{9d}\u{9e}\u{178}",
    "\u{a0}\u{a1}\u{a2}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}\u{a8}\u{a9}\u{aa}\u{ab}\u{ac}\u{ad}\u{ae}\u{af}\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}\u{b8}\u{b9}\u{ba}\u{bb}\u{bc}\u{bd}\u{be}\u{bf}",
    "\u{c0}\u{c1}\u{c2}\u{c3}\u{c4}\u{c5}\u{c6}\u{c7}\u{c8}\u{c9}\u{ca}\u{cb}\u{cc}\u{cd}\u{ce}\u{cf}\u{11e}\u{d1}\u{d2}\u{d3}\u{d4}\u{d5}\u{d6}\u{d7}\u{d8}\u{d9}\u{da}\u{db}\u{dc}\u{130}\u{15e}\u{df}",
    "\u{e0}\u{e1}\u{e2}\u{e3}\u{e4}\u{e5}\u{e6}\u{e7}\u{e8}\u{e9}\u{ea}\u{eb}\u{ec}\u{ed}\u{ee}\u{ef}\u{11f}\u{f1}\u{f2}\u{f3}\u{f4}\u{f5}\u{f6}\u{f7}\u{f8}\u{f9}\u{fa}\u{fb}\u{fc}\u{131}\u{15f}\u{ff}",
];

/// The high half of Hebrew.
const CP1255: HighHalf = [
    "\u{20ac}\u{81}\u{201a}\u{192}\u{201e}\u{2026}\u{2020}\u{2021}\u{2c6}\u{2030}\u{8a}\u{2039}\u{8c}\u{8d}\u{8e}\u{8f}\u{90}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{2dc}\u{2122}\u{9a}\u{203a}\u{9c}\u{9d}\u{9e}\u{9f}",
    "\u{a0}\u{a1}\u{a2}\u{a3}\u{20aa}\u{a5}\u{a6}\u{a7}\u{a8}\u{a9}\u{d7}\u{ab}\u{ac}\u{ad}\u{ae}\u{af}\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}\u{b8}\u{b9}\u{f7}\u{bb}\u{bc}\u{bd}\u{be}\u{bf}",
    "\u{5b0}\u{5b1}\u{5b2}\u{5b3}\u{5b4}\u{5b5}\u{5b6}\u{5b7}\u{5b8}\u{5b9}\u{ca}\u{5bb}\u{5bc}\u{5bd}\u{5be}\u{5bf}\u{5c0}\u{5c1}\u{5c2}\u{5c3}\u{5f0}\u{5f1}\u{5f2}\u{5f3}\u{5f4}\u{d9}\u{da}\u{db}\u{dc}\u{dd}\u{de}\u{df}",
    "\u{5d0}\u{5d1}\u{5d2}\u{5d3}\u{5d4}\u{5d5}\u{5d6}\u{5d7}\u{5d8}\u{5d9}\u{5da}\u{5db}\u{5dc}\u{5dd}\u{5de}\u{5df}\u{5e0}\u{5e1}\u{5e2}\u{5e3}\u{5e4}\u{5e5}\u{5e6}\u{5e7}\u{5e8}\u{5e9}\u{5ea}\u{fb}\u{fc}\u{200e}\u{200f}\u{ff}",
];

/// The high half of Arabic.
const CP1256: HighHalf = [
    "\u{20ac}\u{67e}\u{201a}\u{192}\u{201e}\u{2026}\u{2020}\u{2021}\u{2c6}\u{2030}\u{679}\u{2039}\u{152}\u{686}\u{698}\u{688}\u{6af}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{6a9}\u{2122}\u{691}\u{203a}\u{153}\u{200c}\u{200d}\u{6ba}",
    "\u{a0}\u{60c}\u{a2}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}\u{a8}\u{a9}\u{6be}\u{ab}\u{ac}\u{ad}\u{ae}\u{af}\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}\u{b8}\u{b9}\u{61b}\u{bb}\u{bc}\u{bd}\u{be}\u{61f}",
    "\u{6c1}\u{621}\u{622}\u{623}\u{624}\u{625}\u{626}\u{627}\u{628}\u{629}\u{62a}\u{62b}\u{62c}\u{62d}\u{62e}\u{62f}\u{630}\u{631}\u{632}\u{633}\u{634}\u{635}\u{636}\u{d7}\u{637}\u{638}\u{639}\u{63a}\u{640}\u{641}\u{642}\u{643}",
    "\u{e0}\u{644}\u{e2}\u{645}\u{646}\u{647}\u{648}\u{e7}\u{e8}\u{e9}\u{ea}\u{eb}\u{649}\u{64a}\u{ee}\u{ef}\u{64b}\u{64c}\u{64d}\u{64e}\u{f4}\u{64f}\u{650}\u{f7}\u{651}\u{f9}\u{652}\u{fb}\u{fc}\u{200e}\u{200f}\u{6d2}",
];

/// The high half of Baltic (Lithuanian, Latvian, Estonian).
const CP1257: HighHalf = [
    "\u{20ac}\u{81}\u{201a}\u{83}\u{201e}\u{2026}\u{2020}\u{2021}\u{88}\u{2030}\u{8a}\u{2039}\u{8c}\u{a8}\u{2c7}\u{b8}\u{90}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{98}\u{2122}\u{9a}\u{203a}\u{9c}\u{af}\u{2db}\u{9f}",
    "\u{a0}\u{a1}\u{a2}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}\u{d8}\u{a9}\u{156}\u{ab}\u{ac}\u{ad}\u{ae}\u{c6}\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}\u{f8}\u{b9}\u{157}\u{bb}\u{bc}\u{bd}\u{be}\u{e6}",
    "\u{104}\u{12e}\u{100}\u{106}\u{c4}\u{c5}\u{118}\u{112}\u{10c}\u{c9}\u{179}\u{116}\u{122}\u{136}\u{12a}\u{13b}\u{160}\u{143}\u{145}\u{d3}\u{14c}\u{d5}\u{d6}\u{d7}\u{172}\u{141}\u{15a}\u{16a}\u{dc}\u{17b}\u{17d}\u{df}",
    "\u{105}\u{12f}\u{101}\u{107}\u{e4}\u{e5}\u{119}\u{113}\u{10d}\u{e9}\u{17a}\u{117}\u{123}\u{137}\u{12b}\u{13c}\u{161}\u{144}\u{146}\u{f3}\u{14d}\u{f5}\u{f6}\u{f7}\u{173}\u{142}\u{15b}\u{16b}\u{fc}\u{17c}\u{17e}\u{2d9}",
];

/// The high half of Vietnamese.
const CP1258: HighHalf = [
    "\u{20ac}\u{81}\u{201a}\u{192}\u{201e}\u{2026}\u{2020}\u{2021}\u{2c6}\u{2030}\u{8a}\u{2039}\u{152}\u{8d}\u{8e}\u{8f}\u{90}\u{2018}\u{2019}\u{201c}\u{201d}\u{2022}\u{2013}\u{2014}\u{2dc}\u{2122}\u{9a}\u{203a}\u{153}\u{9d}\u{9e}\u{178}",
    "\u{a0}\u{a1}\u{a2}\u{a3}\u{a4}\u{a5}\u{a6}\u{a7}\u{a8}\u{a9}\u{aa}\u{ab}\u{ac}\u{ad}\u{ae}\u{af}\u{b0}\u{b1}\u{b2}\u{b3}\u{b4}\u{b5}\u{b6}\u{b7}\u{b8}\u{b9}\u{ba}\u{bb}\u{bc}\u{bd}\u{be}\u{bf}",
    "\u{c0}\u{c1}\u{c2}\u{102}\u{c4}\u{c5}\u{c6}\u{c7}\u{c8}\u{c9}\u{ca}\u{cb}\u{300}\u{cd}\u{ce}\u{cf}\u{110}\u{d1}\u{309}\u{d3}\u{d4}\u{1a0}\u{d6}\u{d7}\u{d8}\u{d9}\u{da}\u{db}\u{dc}\u{1af}\u{303}\u{df}",
    "\u{e0}\u{e1}\u{e2}\u{103}\u{e4}\u{e5}\u{e6}\u{e7}\u{e8}\u{e9}\u{ea}\u{eb}\u{301}\u{ed}\u{ee}\u{ef}\u{111}\u{f1}\u{323}\u{f3}\u{f4}\u{1a1}\u{f6}\u{f7}\u{f8}\u{f9}\u{fa}\u{fb}\u{fc}\u{1b0}\u{20ab}\u{ff}",
];

/// The high half of KOI8-R (the other Russian encoding).
const KOI8R: HighHalf = [
    "\u{2500}\u{2502}\u{250c}\u{2510}\u{2514}\u{2518}\u{251c}\u{2524}\u{252c}\u{2534}\u{253c}\u{2580}\u{2584}\u{2588}\u{258c}\u{2590}\u{2591}\u{2592}\u{2593}\u{2320}\u{25a0}\u{2219}\u{221a}\u{2248}\u{2264}\u{2265}\u{a0}\u{2321}\u{b0}\u{b2}\u{b7}\u{f7}",
    "\u{2550}\u{2551}\u{2552}\u{451}\u{2553}\u{2554}\u{2555}\u{2556}\u{2557}\u{2558}\u{2559}\u{255a}\u{255b}\u{255c}\u{255d}\u{255e}\u{255f}\u{2560}\u{2561}\u{401}\u{2562}\u{2563}\u{2564}\u{2565}\u{2566}\u{2567}\u{2568}\u{2569}\u{256a}\u{256b}\u{256c}\u{a9}",
    "\u{44e}\u{430}\u{431}\u{446}\u{434}\u{435}\u{444}\u{433}\u{445}\u{438}\u{439}\u{43a}\u{43b}\u{43c}\u{43d}\u{43e}\u{43f}\u{44f}\u{440}\u{441}\u{442}\u{443}\u{436}\u{432}\u{44c}\u{44b}\u{437}\u{448}\u{44d}\u{449}\u{447}\u{44a}",
    "\u{42e}\u{410}\u{411}\u{426}\u{414}\u{415}\u{424}\u{413}\u{425}\u{418}\u{419}\u{41a}\u{41b}\u{41c}\u{41d}\u{41e}\u{41f}\u{42f}\u{420}\u{421}\u{422}\u{423}\u{416}\u{412}\u{42c}\u{42b}\u{417}\u{428}\u{42d}\u{429}\u{427}\u{42a}",
];

/// Decode bytes in one of the codepages above.
fn decode_with(data: &[u8], table: HighHalf) -> String {
    let mut out = String::with_capacity(data.len());
    for &byte in data {
        if byte < 0x80 {
            out.push(byte as char);
        } else {
            let index = (byte - 0x80) as usize;
            let chunk = index / 32;
            let offset = index % 32;
            // The chunks are 32 characters each; a byte past the table's
            // end cannot happen (the table is exactly 128 long), but the
            // fallback keeps the decode total.
            let piece = table.get(chunk).copied().unwrap_or("");
            match piece.chars().nth(offset) {
                Some(c) => out.push(c),
                None => out.push(byte as char),
            }
        }
    }
    out
}

/// Decode Central European (Polish, Czech, Hungarian).
pub fn decode_cp1250(data: &[u8]) -> String {
    decode_with(data, CP1250)
}

/// Decode Cyrillic (the Russian FictionBook corpus).
pub fn decode_cp1251(data: &[u8]) -> String {
    decode_with(data, CP1251)
}

/// Decode Western European (the HTML/RTF default).
pub fn decode_cp1252(data: &[u8]) -> String {
    decode_with(data, CP1252)
}

/// Decode Greek.
pub fn decode_cp1253(data: &[u8]) -> String {
    decode_with(data, CP1253)
}

/// Decode Turkish.
pub fn decode_cp1254(data: &[u8]) -> String {
    decode_with(data, CP1254)
}

/// Decode Hebrew.
pub fn decode_cp1255(data: &[u8]) -> String {
    decode_with(data, CP1255)
}

/// Decode Arabic.
pub fn decode_cp1256(data: &[u8]) -> String {
    decode_with(data, CP1256)
}

/// Decode Baltic (Lithuanian, Latvian, Estonian).
pub fn decode_cp1257(data: &[u8]) -> String {
    decode_with(data, CP1257)
}

/// Decode Vietnamese.
pub fn decode_cp1258(data: &[u8]) -> String {
    decode_with(data, CP1258)
}

/// Decode KOI8-R (the other Russian encoding).
pub fn decode_koi8_r(data: &[u8]) -> String {
    decode_with(data, KOI8R)
}

/// Decode bytes in the codepage an XML declaration or an RTF control
/// word names. `None` when the name is one this crate does not know —
/// the caller then falls back to UTF-8, which never fails.
pub fn decode_named(name: &str, data: &[u8]) -> Option<String> {
    let key = name
        .trim()
        .to_ascii_lowercase()
        .replace(['_', ' '], "-");
    Some(match key.as_str() {
        // 65001 is how an RTF \ansicpg names UTF-8.
        "utf-8" | "utf8" | "65001" => String::from_utf8_lossy(data).into_owned(),
        "windows-1250" | "cp1250" | "1250" => decode_cp1250(data),
        "windows-1251" | "cp1251" | "1251" => decode_cp1251(data),
        "windows-1252" | "cp1252" | "1252" => decode_cp1252(data),
        "windows-1253" | "cp1253" | "1253" => decode_cp1253(data),
        "windows-1254" | "cp1254" | "1254" => decode_cp1254(data),
        "windows-1255" | "cp1255" | "1255" => decode_cp1255(data),
        "windows-1256" | "cp1256" | "1256" => decode_cp1256(data),
        "windows-1257" | "cp1257" | "1257" => decode_cp1257(data),
        "windows-1258" | "cp1258" | "1258" => decode_cp1258(data),
        "koi8-r" | "koi8r" | "koi8" => decode_koi8_r(data),
        "iso-8859-1" | "iso8859-1" | "latin1" | "latin-1" | "iso-latin1" => {
            super::bookdoc::decode_latin1(data)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decodes the corpus actually needs, with the byte's own answer
    /// known: a wrong table here reads wrong for the whole book, and no
    /// test would notice unless the test knew the right letter.
    #[test]
    fn cyrillic_codepages_map_the_alphabet() {
        // cp1251: 0xE0 = а, the alphabet's first letter, 0xC0 = А.
        assert_eq!(decode_cp1251(&[0xEF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]), "привет");
        assert_eq!(decode_cp1251(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]), "Привет");
        // ё at 0xB8, Ё at 0xA8 — the two letters a wrong table turns into
        // the wrong letter.
        assert_eq!(decode_cp1251(&[0xB8]), "ё");
        assert_eq!(decode_cp1251(&[0xA8]), "Ё");
    }

    #[test]
    fn western_punctuation_survives() {
        // The 0x80–0x9F block is the punctuation RTF and Windows documents
        // carry: 0x92 ’ 0x93 “ 0x94 ” 0x97 — 0xA0 a no-break space.
        assert_eq!(decode_cp1252(&[0x92]), "\u{2019}");
        assert_eq!(decode_cp1252(&[0x93, 0x94, 0x97]), "\u{201C}\u{201D}\u{2014}");
        assert_eq!(decode_cp1252(&[0xA0]), "\u{00A0}");
        // The five undefined positions keep their Latin-1 glyph rather
        // than becoming a replacement character.
        assert_eq!(decode_cp1252(&[0x81]), "\u{81}");
    }

    #[test]
    fn central_european_maps_its_accented_letters() {
        // ł (0xB3), ż (0xBF) — a Polish sentence's whole sound.
        assert_eq!(decode_cp1250(&[0x42, 0x79, 0x42, 0xB3]), "ByBł");
        assert_eq!(decode_cp1250(&[0xBF, 0x63]), "żc");
    }

    #[test]
    fn greek_and_turkish_letters() {
        assert_eq!(decode_cp1253(&[0x31]), "1");
        assert_eq!(decode_cp1253(&[0xE1]), "\u{03B1}"); // α
        assert_eq!(decode_cp1254(&[0xFD]), "\u{0131}"); // ı, Turkish dotless i
        assert_eq!(decode_cp1254(&[0xFE]), "\u{015F}"); // ş
    }

    #[test]
    fn koi8_r_is_the_other_cyrillic() {
        // KOI8-R's high half is box-drawing and the alphabet — and its
        // letters sit AT OTHER BYTE POSITIONS than cp1251's: 0xF0 = П
        // here, 0xEF there.
        assert_eq!(decode_koi8_r(&[0xF0]), "П");
        assert_eq!(decode_koi8_r(&[0xD0]), "п");
    }

    #[test]
    fn ascii_passes_through_every_codepage() {
        for decode in [
            decode_cp1250 as fn(&[u8]) -> String,
            decode_cp1251, decode_cp1252, decode_cp1253, decode_cp1254,
            decode_cp1255, decode_cp1256, decode_cp1257, decode_cp1258,
            decode_koi8_r,
        ] {
            assert_eq!(decode(b"Chapter 7: the letter"), "Chapter 7: the letter");
        }
    }

    #[test]
    fn a_named_encoding_resolves_to_its_decoder() {
        let cyrillic = &[0xEF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2]; // "привет"
        assert_eq!(decode_named("windows-1251", cyrillic), Some("привет".to_string()));
        assert_eq!(decode_named("cp1251", cyrillic), Some("привет".to_string()));
        assert_eq!(decode_named("utf-8", "héllo".as_bytes()), Some("héllo".to_string()));
        assert_eq!(decode_named("iso-8859-1", &[0xE9]), Some("é".to_string()));
        // An encoding this crate does not carry (UTF-16, EBCDIC) is None:
        // the caller falls back to UTF-8, which never fails.
        assert_eq!(decode_named("utf-16", &[1, 2, 3]), None);
        assert_eq!(decode_named("shift_jis", &[1, 2, 3]), None);
    }

    #[test]
    fn an_undefined_byte_keeps_its_latin1_glyph() {
        // Windows-1252's five undefined positions (0x81, 0x8D, 0x8F,
        // 0x90, 0x9D) have no meaning; the decode keeps the Latin-1
        // control code rather than turning every one of them into the
        // same replacement character. The reply is total: no byte can
        // fail, so no file can crash the decode.
        assert_eq!(decode_cp1252(&[0x8D]), "\u{8D}");
        assert_eq!(decode_cp1252(&[0x81, 0x8F, 0x90, 0x9D]), "\u{81}\u{8F}\u{90}\u{9D}");
        // cp1251 maps those bytes to Cyrillic letters instead — the
        // codepages do not agree, which is the whole point of the table.
        assert_eq!(decode_cp1251(&[0x8D]), "\u{40C}");
    }
}
