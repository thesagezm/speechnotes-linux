//! The slide decks' tests: a pptx fixture built here (not on disk), an odp
//! fixture likewise, and the contracts both must satisfy — the slide
//! count, the speakable text, the byte spans, the media index.


//  the two ODF fixtures' raw strings gained a second hash each — the
// original had them as r#"…"#, but the fixtures' own `fill-color="#…"` byte
// sequences terminate that raw string early (the errors the original produces
// start exactly at those bytes); r##"…"## is the minimal faithful form.
use crate::slides::{Content, SlideDeck, SlideError};

/// A minimal but complete pptx package: one slide with a title and a body
/// placeholder, built as a ZIP in memory.
fn pptx_fixture() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    let mut add = |name: &str, body: &str| {
        zip.start_file(name, opts).expect("start");
        std::io::Write::write_all(&mut zip, body.as_bytes()).expect("write");
    };
    add(
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slideMasters/slideMaster1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml"/>
<Override PartName="/ppt/slideLayouts/slideLayout1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"#,
    );
    add(
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"#,
    );
    add(
        "ppt/_rels/presentation.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="slideMasters/slideMaster1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
</Relationships>"#,
    );
    add(
        "ppt/presentation.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:presentation xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
<p:sldMasterIdLst><p:sldMasterId id="2147483648" r:id="rId1"/></p:sldMasterIdLst>
<p:sldIdLst><p:sldId id="256" r:id="rId2"/></p:sldIdLst>
<p:sldSz cx="9144000" cy="5143500"/>
</p:presentation>"#,
    );
    add(
        "ppt/slideMasters/slideMaster1.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldMaster xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld>
<p:sldLayoutIdLst><p:sldLayoutId id="2147483649" r:id="rId1"/></p:sldLayoutIdLst>
</p:sldMaster>"#,
    );
    add(
        "ppt/slideMasters/_rels/slideMaster1.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>
</Relationships>"#,
    );
    add(
        "ppt/slideLayouts/slideLayout1.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sldLayout xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" type="obj" preserve="1">
<p:cSld><p:spTree><p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/></p:spTree></p:cSld>
</p:sldLayout>"#,
    );
    add(
        "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideMaster" Target="../slideMasters/slideMaster1.xml"/>
</Relationships>"#,
    );
    add(
        "ppt/slides/slide1.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
<p:cSld><p:spTree>
<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr/>
<p:sp>
<p:nvSpPr><p:cNvPr id="2" name="Title 1"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr>
<p:spPr><a:xfrm><a:off x="457200" y="274638"/><a:ext cx="8229600" cy="1143000"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>
<p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US" sz="4400" b="1"/><a:t>The Deck Title</a:t></a:r></a:p></p:txBody>
</p:sp>
<p:sp>
<p:nvSpPr><p:cNvPr id="3" name="Content 2"/><p:cNvSpPr><a:spLocks noGrp="1"/></p:cNvSpPr><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr>
<p:spPr><a:xfrm><a:off x="457200" y="1600200"/><a:ext cx="8229600" cy="3086100"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr>
<p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:r><a:rPr lang="en-US" sz="2400"/><a:t>First bullet point</a:t></a:r></a:p><a:p><a:r><a:rPr lang="en-US" sz="2400"/><a:t>Second bullet point</a:t></a:r></a:p></p:txBody>
</p:sp>
</p:spTree></p:cSld>
</p:sld>"#,
    );
    add(
        "ppt/slides/_rels/slide1.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/>
</Relationships>"#,
    );
    zip.finish().expect("finish").into_inner()
}

/// A minimal odp package: one slide, a title frame and a body frame with
/// two paragraphs, plus a page layout sizing the slide.
fn odp_fixture() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    let mut add = |name: &str, body: &str| {
        zip.start_file(name, opts).expect("start");
        std::io::Write::write_all(&mut zip, body.as_bytes()).expect("write");
    };
    add(
        "mimetype",
        "application/vnd.oasis.opendocument.presentation",
    );
    add(
        "content.xml",
        r##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" office:version="1.3">
<office:automatic-styles>
<style:style style:name="T1" style:family="text"><style:text-properties fo:font-size="24pt" fo:font-weight="bold" fo:color="#102040"/></style:style>
<style:style style:name="P1" style:family="paragraph"><style:paragraph-properties fo:text-align="center"/></style:style>
<style:style style:name="G1" style:family="graphic"><style:graphic-properties draw:fill="solid" draw:fill-color="#ff8800" svg:stroke-color="#333333" svg:stroke-width="0.06cm"/></style:style>
</office:automatic-styles>
<office:body>
<office:presentation>
<draw:page draw:name="Slide 1" draw:master-page-name="Standard">
<draw:frame presentation:class="title" svg:x="1cm" svg:y="1cm" svg:width="20cm" svg:height="3cm" draw:style-name="G1">
<draw:text-box><text:p text:style-name="P1"><text:span text:style-name="T1">The Deck Title</text:span></text:p></draw:text-box>
</draw:frame>
<draw:frame presentation:class="outline" svg:x="1cm" svg:y="5cm" svg:width="20cm" svg:height="8cm">
<draw:text-box>
<text:p><text:span>First bullet point</text:span></text:p>
<text:p><text:span>Second bullet point</text:span></text:p>
</draw:text-box>
</draw:frame>
<presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>Speaker note here</text:p></draw:text-box></draw:frame></presentation:notes>
</draw:page>
</office:presentation>
</office:body>
</office:document-content>"##,
    );
    add(
        "styles.xml",
        r##"<?xml version="1.0" encoding="UTF-8"?>
<office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0">
<office:automatic-styles>
<style:page-layout style:name="PM1"><style:page-layout-properties fo:page-width="28cm" fo:page-height="15.75cm"/></style:page-layout>
<style:style style:name="dp1" style:family="drawing-page"><style:drawing-page-properties draw:fill="solid" draw:fill-color="#f0f0f0"/></style:style>
</office:automatic-styles>
<office:master-styles>
<style:master-page style:name="Standard" style:page-layout-name="PM1" draw:style-name="dp1"/>
</office:master-styles>
</office:document-styles>"##,
    );
    zip.finish().expect("finish").into_inner()
}

#[test]
fn a_pptx_fixture_parses_into_slides_with_text_and_spans() {
    let bytes = pptx_fixture();
    assert!(deckcraft_pptx::sniff(&bytes), "the fixture is an OOXML package");
    let deck = SlideDeck::parse(&bytes).expect("parse");
    assert_eq!(deck.slides.len(), 1);
    let slide = &deck.slides[0];
    assert_eq!(slide.title, "The Deck Title");
    assert!(
        slide.text.contains("First bullet point"),
        "the body text is speakable: {:?}",
        slide.text
    );
    assert!(slide.text.contains("Second bullet point"));
    // The spans index into the slide's own text, and each names the item
    // that shows it.
    assert!(!slide.spans.is_empty());
    for span in &slide.spans {
        assert!(span.end <= slide.text.len());
        assert!(span.start < span.end);
        if span.item != usize::MAX {
            assert!(span.item < slide.items.len());
        }
        let slice = &slide.text[span.start..span.end];
        assert!(!slice.trim().is_empty(), "a span covers real text");
    }
    // The title's span lands on a text item with a paragraph.
    let title_span = slide
        .spans
        .iter()
        .find(|s| &slide.text[s.start..s.end] == "The Deck Title")
        .expect("the title has a span");
    assert!(title_span.para.is_some());
    let item = &slide.items[title_span.item];
    assert!(matches!(item.content, Content::Text { .. }));
}

#[test]
fn an_odp_fixture_parses_into_the_same_model() {
    let bytes = odp_fixture();
    assert!(crate::odp::sniffs_like_odp(&bytes));
    let deck = SlideDeck::parse(&bytes).expect("parse");
    assert_eq!(deck.slides.len(), 1);
    let slide = &deck.slides[0];
    assert_eq!(slide.title, "The Deck Title");
    assert!(slide.text.contains("First bullet point"), "{:?}", slide.text);
    assert!(slide.text.contains("Second bullet point"));
    assert_eq!(slide.notes, "Speaker note here");
    // The page size comes from the page layout: 28cm × 15.75cm in points.
    assert!((deck.w - 28.0 * 72.0 / 2.54).abs() < 0.5, "width: {}", deck.w);
    assert!((deck.h - 15.75 * 72.0 / 2.54).abs() < 0.5, "height: {}", deck.h);
    // The master's background paint reaches the slide.
    assert_eq!(slide.bg.as_deref(), Some("#f0f0f0"));
    // The title's paragraph resolves through its text style.
    let title_span = slide
        .spans
        .iter()
        .find(|s| &slide.text[s.start..s.end] == "The Deck Title")
        .expect("the title has a span");
    let item = &slide.items[title_span.item];
    let Content::Text { paras, .. } = &item.content else {
        panic!("the title is a text item");
    };
    let run = &paras[0].runs[0];
    assert_eq!(run.size, 24.0);
    assert!(run.bold);
    assert_eq!(run.color.as_deref(), Some("#102040"));
    assert_eq!(paras[0].align, "center");
}

#[test]
fn bytes_that_are_no_deck_are_refused() {
    let err = SlideDeck::parse(b"not a deck at all").unwrap_err();
    assert!(matches!(err, SlideError::NotADeck(_)), "{err}");
}
