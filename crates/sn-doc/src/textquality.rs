//! Judging a PDF page's text layer before the reader speaks it.
//!
//! A text layer being *present* says nothing about it being *right*. A page
//! can carry a layer that is OCR junk baked in by whatever tool made the PDF
//! (the GTD deck's Tanner-stages figure reads `~~ -·~~ ) ~ ✓i (`), a font
//! whose ToUnicode map was never written, or glyphs mapped into the private
//! use area. Speaking that verbatim is worse than speaking nothing: the
//! voice reads tildes aloud. The gate scores the extracted text and lets the
//! caller demote a bad layer to OCR — the same fallback a page with no
//! layer at all already takes.
//!
//! The scoring is deliberately conservative: a false "garbage" costs an OCR
//! pass (once — the store caches it) and the OCR of a good page reads much
//! like its layer; a false "fine" costs the user hearing junk. So the rules
//! fire only on text that is confidently bad, and a page too short to judge
//! keeps its layer.

/// One page's text-layer score: the raw counts behind the garbage verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct LayerQuality {
    /// Non-whitespace characters — everything is scored against this.
    pub nonspace: usize,
    /// Alphabetic characters, in any script.
    pub letters: usize,
    /// Private-use, replacement and other prose-impossible characters over
    /// [`Self::nonspace`].
    pub junk_ratio: f64,
    /// [`Self::letters`] over [`Self::nonspace`]. Real prose sits above
    /// ~0.7; a page of OCR-junk symbols sinks far below it.
    pub letter_ratio: f64,
    /// Whitespace-delimited tokens (two letters or more) that are common
    /// words, over all such tokens — `None` when the page's letters are not
    /// mostly Latin script, where an English-leaning wordlist proves
    /// nothing and the rule must not fire.
    pub real_word_ratio: Option<f64>,
}

/// Relative junk-character share at which the layer is junk outright. The
/// bar is low because honest text never contains these characters in bulk —
/// a page of real prose scores under 0.02, a real label page zero — while
/// OCR-junk layers are largely made of them. The real Tanner-figure junk
/// layer (7 letters, 40 characters) scores 0.15: too few letters for the
/// letter-ratio rule, but a sixth of the page is tildes.
const JUNK_RATIO_LIMIT: f64 = 0.10;

/// A page needs this many non-whitespace characters before its junk ratio
/// can condemn it — a fragment of three characters is not a measurement.
const MIN_NONSPACE_FOR_JUNK: usize = 8;

/// A page needs this many letters before its letter ratio can condemn it.
/// Below that, "mostly symbols" may be an honest label page or a page
/// number, and OCRing it buys nothing.
const MIN_LETTERS_FOR_RATIO: usize = 12;

/// Letter ratio below which a page of this many letters is symbol soup.
/// Real prose sits above 0.7; digit-heavy tables stay above 0.45. The
/// Tanner-figure junk layer scores 0.14.
const LETTER_RATIO_LIMIT: f64 = 0.35;

/// A page needs this many letters before its real-word ratio can condemn
/// it: enough tokens that "no common word at all" is a measurement, not a
/// small-sample accident. High, deliberately: short jargon pages (a hormone
/// table, a label figure) have no common words either, and they are honest.
const MIN_LETTERS_FOR_WORDS: usize = 300;

/// Real-word ratio below which a page of Latin script is judged decoded-to
/// garbage rather than written in a language the wordlist does not know.
/// The bar is near-zero because common words are near-unavoidable: real
/// prose in any of the list's languages scores above 0.15, jargon-heavy
/// label pages still score above 0.03, and a wrong ToUnicode map scores 0.
const REAL_WORD_LIMIT: f64 = 0.02;

/// The characters that count as junk: the private use area (a font mapped
/// nowhere), the Unicode replacement character, non-whitespace control
/// characters, and the handful of symbols OCR junk layers are made of.
/// Deliberately narrow — `▪` is a real bullet in a real layer, so bullets
/// are not here; what the set misses, the letter ratio catches.
fn is_junk(c: char) -> bool {
    matches!(c, '\u{E000}'..='\u{F8FF}')
        || c == '\u{FFFD}'
        || (c.is_control() && !c.is_whitespace())
        || matches!(c, '~' | '·' | '✓' | '§' | '¤' | '¬' | '¦')
}

/// The common words of the Latin-script languages this box's readers are
/// likely to meet. A page of real prose — in any of them — is thick with
/// these; a layer whose ToUnicode map points at the wrong glyphs produces
/// letter salad that contains none. Sorted byte-wise, for binary search —
/// held honest by the sortedness test below.
const COMMON_WORDS: &[&str] = &[
    "about", "after", "all", "also", "an", "and", "any", "are", "as", "at", "auf", "aus", "avec",
    "be", "been", "bei", "but", "by", "can", "che", "come", "como", "con", "could", "dans", "das",
    "de", "del", "della", "dem", "den", "der", "des", "di", "die", "do", "ein", "eine", "el",
    "elle", "en", "es", "est", "et", "even", "for", "from", "für", "gli", "had", "has", "have",
    "he", "her", "his", "how", "il", "im", "in", "into", "is", "ist", "it", "its", "la", "le",
    "les", "like", "lo", "mais", "make", "mas", "may", "mit", "more", "most", "my", "nicht", "no",
    "non", "not", "nous", "não", "of", "on", "one", "only", "or", "os", "other", "our", "out",
    "over", "par", "para", "pas", "per", "plus", "por", "pour", "que", "qui", "se", "see", "she",
    "should", "si", "so", "some", "son", "sono", "su", "such", "sur", "than", "that", "the",
    "their", "them", "then", "there", "they", "this", "time", "to", "un", "una", "une", "up",
    "us", "use", "von", "was", "way", "we", "were", "what", "when", "which", "who", "will",
    "with", "would", "you", "your",
];

/// Whether a token is one of the common words. Case-insensitive; a token
/// with letters beyond A–Z (accents, umlauts) matches only its exact
/// lowercase spelling, which the list carries where it matters (`für`,
/// `não`).
fn is_common_word(token: &str) -> bool {
    let lower = token.to_lowercase();
    COMMON_WORDS.binary_search(&lower.as_str()).is_ok()
}

/// Score a page's extracted text. Cheap — one pass over the characters —
/// so every read path can afford it.
pub fn layer_quality(text: &str) -> LayerQuality {
    let mut nonspace = 0usize;
    let mut letters = 0usize;
    let mut junk = 0usize;
    let mut latin_letters = 0usize;
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        nonspace += 1;
        if c.is_alphabetic() {
            letters += 1;
            if c.is_ascii() {
                latin_letters += 1;
            }
        }
        if is_junk(c) {
            junk += 1;
        }
    }
    let ratio = |n: usize| {
        if nonspace == 0 {
            0.0
        } else {
            n as f64 / nonspace as f64
        }
    };
    // The word rule only speaks for Latin-script pages. A page of Chinese,
    // Arabic or Cyrillic is alphabetic enough to read cleanly from its
    // layer, and "no English words in it" is the rule's blindness, not the
    // layer's fault.
    let latin_dominant = letters > 0 && latin_letters * 20 > letters * 19;
    let real_word_ratio = if latin_dominant {
        let mut tokens = 0usize;
        let mut hits = 0usize;
        for token in text.split(|c: char| !c.is_alphanumeric()) {
            let letter_count = token.chars().filter(|c| c.is_alphabetic()).count();
            if letter_count >= 2 {
                tokens += 1;
                if is_common_word(token) {
                    hits += 1;
                }
            }
        }
        if tokens == 0 {
            None
        } else {
            Some(hits as f64 / tokens as f64)
        }
    } else {
        None
    };
    LayerQuality {
        nonspace,
        letters,
        junk_ratio: ratio(junk),
        letter_ratio: ratio(letters),
        real_word_ratio,
    }
}

impl LayerQuality {
    /// Whether the layer is confidently bad and worth demoting to OCR.
    /// Three independent rules — symbol junk, letter-starved text, and
    /// Latin text with no common words at all — because the ways a layer
    /// goes wrong are themselves independent: baked-in OCR junk, fonts
    /// mapped into the private use area, and ToUnicode tables pointing at
    /// the wrong glyphs.
    pub fn is_garbage(&self) -> bool {
        if self.nonspace >= MIN_NONSPACE_FOR_JUNK && self.junk_ratio >= JUNK_RATIO_LIMIT {
            return true;
        }
        if self.letters >= MIN_LETTERS_FOR_RATIO && self.letter_ratio < LETTER_RATIO_LIMIT {
            return true;
        }
        if let Some(ratio) = self.real_word_ratio {
            if self.letters >= MIN_LETTERS_FOR_WORDS && ratio < REAL_WORD_LIMIT {
                return true;
            }
        }
        false
    }
}

/// The one OCR decision the read path and the warm-up share: the layer read
/// nothing at all, or what it read is confidently bad (`is_garbage`) — the
/// same bar a scanner watermark stub fails. Empty text is judged separately:
/// `layer_quality("")`'s rules all carry minimum-length bars, so an empty
/// layer would otherwise sail through as clean.
//  authored from the call sites (sn-store books.rs "empty, junk, or
// watermark" comment + the gated second-opinion flow) — no original of this
// function surfaced.
pub fn layer_needs_ocr(text: &str) -> bool {
    text.trim().is_empty() || layer_quality(text).is_garbage()
}

/// The prose-density gap at which a second reading of a page counts as
/// materially better than the first.
///
/// Measured against real pages (the scanned "GESTATIONAL TROPHOBLASTIC
/// DISEASES" book, whose producer baked a shift-OCR layer into every page):
/// the good recognition scores a letter ratio of 0.956–0.975 where the
/// baked layer scores 0.644–0.786 — a gap of 0.18 to 0.33. A real text
/// PDF's own layer and the recognition of the same page score within 0.006
/// of each other, because both are reading the same real text. The bar
/// sits at 0.08, an order of magnitude above the noise and an order of
/// magnitude below the signal.
const PROSE_DENSITY_GAIN: f64 = 0.08;

/// The real-word share at which a reading is worth speaking even though the
/// other one has none: a wrong ToUnicode map produces letter salad that
/// passes the letter-ratio rule (it is all letters) but speaks no language,
/// and a fresh recognition of the same page does.
const REAL_WORD_FLOOR: f64 = 0.08;

/// Whether a second, independent reading of the same page reads materially
/// better than the first — the verdict that lets the reader *drop* a text
/// layer instead of speaking it before the good reading.
///
/// **Why this exists.** A PDF can carry a text layer that is present, is
/// not confidently junk by the gate's rules, and is nevertheless unreadable:
/// the producer's own OCR, shifted a character out of place ("n1ay-be the
/// catJse of bladder, b0wel", "Sktttfot-assessr11ent"). The gate cannot
/// condemn it — it is full of real words, just the wrong ones — so the
/// read path folded the good recognition *under* it, and the reader heard
/// the garbage first and the truth second. The fix is not a wider gate but
/// a comparison: two readings of the same page, one of which is
/// demonstrably more prose-like, and the better one stands alone.
///
/// **The two rules.** Prose density first (the recognition is letters where
/// the layer is symbols and stray glyphs), and vocabulary second (the layer
/// speaks no real words at all while the recognition does — letter salad
/// from a broken ToUnicode map).
///
/// **What it must not do.** A false "better" throws away a good layer, and
/// the layer is the better reading wherever both are honest: it has the
/// document's own reading order and structure, and it costs nothing. So
/// the second reading has to be a reading in its own right first (enough
/// letters, not itself junk), and the comparison is always between the
/// two readings of the *same* page — never against a fixed bar.
pub fn reading_beats(base: &str, other: &str) -> bool {
    let b = layer_quality(base);
    let o = layer_quality(other);
    // The other reading must be a reading at all: a page no engine could
    // read must not displace a layer that could.
    if o.letters < MIN_LETTERS_FOR_RATIO || o.is_garbage() {
        return false;
    }
    // Prose density: the other reading is letters where the base is not.
    if o.letter_ratio >= b.letter_ratio + PROSE_DENSITY_GAIN {
        return true;
    }
    // Vocabulary: the base says nothing the wordlist knows and the other
    // reading says plenty. Only for Latin script on both sides — the rule
    // must not fire where the wordlist knows nothing.
    if let (Some(br), Some(or)) = (b.real_word_ratio, o.real_word_ratio) {
        if br < REAL_WORD_LIMIT && or >= REAL_WORD_FLOOR {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_word_list_is_sorted_for_binary_search() {
        assert!(
            COMMON_WORDS.windows(2).all(|w| w[0] <= w[1]),
            "unsorted at: {:?}",
            COMMON_WORDS
                .windows(2)
                .find(|w| w[0] > w[1])
                .map(|w| (w[0], w[1]))
        );
    }

    #[test]
    fn a_real_paragraph_is_kept() {
        let text = "As a result of the dilatation of the cervix, the cervical plug \
                    is lost. This is a blood stained mucoid discharge a few hours \
                    before labour starts, and it contains 40-50 mm Hg of pressure.";
        let q = layer_quality(text);
        assert!(!q.is_garbage(), "{q:?}");
        assert!(q.letter_ratio > 0.7);
        assert!(q.real_word_ratio.unwrap() > 0.2);
    }

    #[test]
    fn the_tanner_figure_junk_layer_is_condemned() {
        // The real page that motivated the gate (GTD deck): OCR junk a
        // producer baked into the PDF as if it were a text layer.
        let text = "P1\n\n~ ~ W .~~· - )~ - ( i i (\n\nP2\n\n~ 0 0\n\nP3\n\n~ ~";
        let q = layer_quality(text);
        assert!(q.is_garbage(), "{q:?}");
    }

    #[test]
    fn a_symbol_soup_layer_is_condemned_by_its_letter_ratio() {
        // Enough stray letters to be judged (12+), drowned in symbols and
        // digits: no junk-set characters, so this is the letter-ratio rule
        // alone catching it.
        let text = "i i i ∆ ∇ ∂ ≈ i i i 42 17 -- ** i i i ∆ ∇ ∂ ≈ i i i 88 0 1 2 ∆ ∇ ≈ ∆ ∇ ≈";
        let q = layer_quality(text);
        assert!(q.letters >= 12);
        assert!(q.junk_ratio == 0.0);
        assert!(q.letter_ratio < 0.35, "{q:?}");
        assert!(q.is_garbage(), "{q:?}");
    }

    #[test]
    fn a_page_mapped_into_the_private_use_area_is_condemned() {
        let text = "\u{E001}\u{E002}\u{E003} \u{E004}\u{E005} \u{E006}\u{E007}\u{E008} \
                    \u{E009}\u{E00A}\u{E00B} \u{E00C}\u{E00D}\u{E00E}\u{E00F}";
        let q = layer_quality(text);
        assert!(q.is_garbage(), "{q:?}");
    }

    #[test]
    fn letter_salad_from_a_wrong_tounicode_map_is_condemned() {
        // A shift-failed ToUnicode: real text's shape, no real words. Long
        // enough (300+ letters) for the word rule to be a measurement.
        let text = "DQH DQG WKH EHIRUH DIWHU ZKHQ WKHB ZHOO WKH VDPH WKHUH \
                    DQG WKHQ ZLOO IURP ZKLFK WKHLU HYHUB PRUQ LQJ DQG HYHQ \
                    VRPH WKLQJ PHDQ ZKLOH VWLOO GRHV JLYH PRVW SHRSOH \
                    EHFDXVH WKHB FRXOG VHH RWKHUV WKDQ WKHQ ORRN RQOB \
                    FRPH LWV RYHU WKLQN DOVR EDFN DIWHU XVH WZR KRZ RXU \
                    EHFRPH EHWZHHQ FKLOG VRFLHWB HBR EHORZ PRVWOB ILUVW \
                    ZRPHQ FKDQJH ILUVW EHEB MOSEB PLJKW EHWZHHQ HDUOB OLNH";
        let q = layer_quality(text);
        assert!(q.letters >= 300, "letters: {}", q.letters);
        assert!(q.real_word_ratio.unwrap() < 0.02, "{q:?}");
        assert!(q.is_garbage(), "{q:?}");
    }

    #[test]
    fn a_short_label_page_keeps_its_layer() {
        // Figure labels and page numbers are honest text; the gate has no
        // business OCRing them.
        assert!(!layer_quality("P1 P2 P3 P4 P5").is_garbage());
        assert!(!layer_quality("12").is_garbage());
        assert!(!layer_quality("Table 2.1").is_garbage());
    }

    #[test]
    fn a_bulleted_real_page_is_not_junk() {
        // `▪` is a real bullet in the GTD deck's real pages.
        let text = "Objectives ▪You should be able to describe the normal \
                    endocrine changes of puberty ▪And list the physical \
                    changes that follow, with the stages involved.";
        assert!(!layer_quality(text).is_garbage());
    }

    #[test]
    fn a_non_latin_page_is_not_condemned_for_its_wordlist() {
        // Chinese text: no Latin words to find — the rule must not fire.
        let text = "妊娠高血压是指怀孕期间出现的高血压症状，通常发生在怀孕二十周之后。\
                    这种情况可能影响母亲和胎儿的健康，需要及时的医疗关注和处理。\
                    定期产前检查可以帮助早期发现和管理这一问题。";
        let q = layer_quality(text);
        assert_eq!(q.real_word_ratio, None);
        assert!(!q.is_garbage(), "{q:?}");
    }

    #[test]
    fn french_and_german_prose_is_kept() {
        let fr = "Le petit prince s'assit, parce qu'il avait été pris par \
                  une si grande fatigue, mais le beau travail doit-il être \
                  surveillé dans les détails pour être fait avec soin.";
        assert!(!layer_quality(fr).is_garbage());
        let de = "Die Qualität dieser Arbeit ist von der Sorgfalt abhängig, \
                  mit der sie ausgeführt wurde, und der Autor hat sie für \
                  seine Leser mit großer Sorgfalt geschrieben.";
        assert!(!layer_quality(de).is_garbage());
    }

    #[test]
    fn an_empty_layer_is_not_the_gate_s_business() {
        // Empty is the caller's existing check; the gate judges content.
        assert!(!layer_quality("").is_garbage());
        assert!(!layer_quality("   \n  ").is_garbage());
    }

    // --- reading_beats: the comparison between two readings of one page ----

    /// The real page from the scanned book ("GESTATIONAL TROPHOBLASTIC
    /// DISEASES", page 965) whose producer baked its own shifted OCR into
    /// the PDF as a text layer: "n1ay-be the catJse", "b0wel",
    /// "Sktttfot-assessr11ent". The layer is full of real words, so the
    /// gate cannot condemn it — and it was being spoken before the good
    /// recognition of the same page.
    const BAKED_LAYER: &str = "Introduction\n\n-\n\n_ • P0P n1ay-be the catJse of bladder, b0wel\n\
        -~--ana sexu al dY-sfunction~\n•-Sktttfot-assessr11ent-and--managemenHs\n\
        re qui re.d__t-o_ens_ure appropriate treatmeot_ __aAd-impr0 ved-outcome-.- - - - ~ - ~ - - ,\n\
        • lnappropr-iate tr.eatmer1t car-, leave women\nworse off- thar,when-they started~\n";

    /// The same page as RapidOCR read it.
    const FRESH_READING: &str = "Introduction\n\n·POP may be the cause of bladder, bowel\n\n\
        andsexual dysfunction.\n\nSkilful assessment and management is required to ensure appropriate treatment\n\n\
        and improved outcome.\n\n·Inappropriate treatment can leavewomen worse off than when they started.";

    #[test]
    fn a_baked_ocr_layer_is_beaten_by_a_fresh_reading() {
        // The gate alone cannot see this one: the layer is mostly letters
        // and carries plenty of real words.
        let layer = layer_quality(BAKED_LAYER);
        assert!(!layer.is_garbage(), "{layer:?}");
        // But the fresh reading of the same page is plainly better prose.
        assert!(reading_beats(BAKED_LAYER, FRESH_READING));
    }

    #[test]
    fn a_fresh_reading_is_not_beaten_by_a_baked_layer() {
        // The comparison is not a coin toss: the good reading wins both
        // ways round, so the layer is never dropped in its favour.
        assert!(!reading_beats(FRESH_READING, BAKED_LAYER));
    }

    #[test]
    fn two_honest_readings_of_the_same_page_stand_even() {
        // A real text PDF: the layer and the recognition of the same page
        // read the same words, so neither beats the other and the mix
        // stays a mix — the layer is the document's own reading order.
        let layer = "As a result of the dilatation of the cervix, the cervical plug \
                    is lost. This is a blood stained mucoid discharge a few hours \
                    before labour starts, and it contains 40-50 mm Hg of pressure. \
                    The membranes are ruptured and the fluid is blood-stained.";
        let ocr = "As a result of the dilatation of the cervix, the cervical plug is lost. \
                   This is a blood stained mucoid discharge a few hours before labour \
                   starts, and it contains 40-50 mm Hg of pressure. The membranes are \
                   ruptured and the fluid is blood-stained.";
        assert!(!reading_beats(layer, ocr), "an honest layer is not displaced");
        assert!(!reading_beats(ocr, layer), "and the other way round");
    }

    #[test]
    fn letter_salad_is_beaten_by_a_reading_that_speaks() {
        // The broken-ToUnicode case: all letters, so the letter-ratio rule
        // passes it, but no real words — the vocabulary rule sees it.
        let salad = "DQH DQG WKH EHIRUH DIWHU ZKHQ WKHB ZHOO WKH VDPH WKHUH \
                    DQG WKHQ ZILOO IURP ZKLFK WKHLU EYHUB PRUQ LQJ DQG HYHQ \
                    VRPH WKLQJ PHDQ ZKLOH VWLOO GRHV JLYH PRVW SHRSOH \
                    EHFDXVH WKHB FRXOG VHH RWKHUV WKDQ WKHQ ORRN RQOB \
                    FRPH LWV RYHU WKLQN DOVR EDFN DIWHU XVH WZR KRW RXU \
                    EHFRPH EHWZHHQ FKLOG VRFLHWB HBR EHORZ PRVWOB ILUVW \
                    ZRPHQ FKDQJH ILUVW EHEB MOSEB PLJKW EHWZHHQ HDUOB OLNH";
        assert!(reading_beats(salad, FRESH_READING));
    }

    #[test]
    fn a_reading_is_never_beaten_by_nothing() {
        // An engine that found nothing must not displace a layer that
        // found something.
        assert!(!reading_beats(BAKED_LAYER, ""));
        assert!(!reading_beats(FRESH_READING, "   \n  "));
    }
}
