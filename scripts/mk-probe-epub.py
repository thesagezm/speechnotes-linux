#!/usr/bin/env python3
"""gamma 2026-10-31: build the speak-from-selection probe EPUB.

The E2E of the selection gesture needs a mouse (dead-ended this session on
the shared desktop), but the gesture's downstream is one production path:
the page posts `speak:<text>` through the `snebook` message handler and
speaksel::on_epub_selection(speak_now=true) starts the chapter session at
the passage. This fixture fires exactly that post from inside the page, so
the handler wiring, the snippet->offset mapping, the session start offset,
the transport and the audio are all exercised for real. The needle sentence
is assembled from fragments so it exists only in the body text, never in
script source.

Scratch fixture only (not shipped).
"""
import json
import os
import struct
import sys
import zipfile

OUT = sys.argv[1] if len(sys.argv) > 1 else "/tmp/speaksel-probe"
BOOK_ID = "AAA00001SPEAKSELPROBE0000000000000000"
SENTENCES = [
    "One marks the opening of the probe chapter.",
    "Two follows without any drama.",
    "Three keeps the pace.",
    "Four adds a little more text to travel through.",
    "Five is halfway to nowhere in particular.",
    "Six continues the padding exercise diligently.",
    "Seven speaks of distant unimportant things.",
    "Eight carries the rhythm forward.",
    "Nine rehearses nothing in particular.",
    "Ten stands at the exact middle of the probe.",
    "Eleven wanders a bit further down the page.",
    "Twelve is the sentence the probe speaks from.",
    "Thirteen comes after the probe target.",
    "Fourteen fills space with quiet confidence.",
    "Fifteen drifts toward the ending.",
    "Sixteen is nearly there.",
    "Seventeen approaches the close.",
    "Eighteen slows down gently.",
    "Nineteen prepares the final words.",
    "Twenty ends the probe chapter.",
]
# (kept simple below — needle assembled from fragments in the page script)


def xhtml(body):
    return (
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<!DOCTYPE html>\n'
        '<html xmlns="http://www.w3.org/1999/xhtml"><head>'
        '<meta charset="utf-8"/><title>Probe</title>'
        # graded page-script test #2: a HEAD script, planting via pure CSS
        # (no DOM body access needed) and via DOMContentLoaded DOM insert.
        '<script type="text/javascript">'
        'try {'
        '  var s = document.createElement("style");'
        '  s.textContent = "body::before{content:\\"HEAD-SCRIPT-RAN\\";'
        'color:#c00;font-size:30px;display:block;}";'
        '  document.documentElement.appendChild(s);'
        '} catch (e) {}'
        'document.addEventListener("DOMContentLoaded", function () {'
        '  try {'
        '    var m = document.createElement("p");'
        '    m.textContent = "PROBE-HEAD-DCL";'
        '    document.body.insertBefore(m, document.body.firstChild);'
        '  } catch (e) {}'
        '});'
        '</script></head>'
        # magenta body: the injected viewer script re-styles the document
        # (body transparent, white html) — a white page proves it ran.
        '<body style="background:#ff00ff">\n'
        + body + "\n</body></html>\n"
    )


paras = "\n".join(f"<p>{s}</p>" for s in SENTENCES)
# The needle, assembled from fragments so it never appears in script source:
# after 5 s the page posts it to the app's real selection handler.
script = """
<script type="text/javascript">
function snPlant(tag, err) {
  try {
    var m = document.createElement("p");
    m.id = "probe-" + tag;
    m.textContent = err ? tag + " ERROR: " + err : tag;
    document.body.insertBefore(m, document.body.firstChild);
  } catch (e) {}
}
try { snPlant("PROBE-SYNC"); } catch (e) { snPlant("PROBE-SYNC", e); }
try {
  var sb = document.createElement("style");
  sb.textContent = "body::after{content:\\"BODY-SCRIPT-RAN\\";color:#00c;font-size:30px;display:block;}";
  document.body.appendChild(sb);
} catch (e) {}
document.addEventListener("DOMContentLoaded", function () {
  try { snPlant("PROBE-DCL"); } catch (e) { snPlant("PROBE-DCL", e); }
});
window.addEventListener("load", function () {
  setTimeout(function () {
    var a = "Twelve is the sent";
    var b = "ence the probe spe";
    var c = "aks from.";
    try { snPlant("PROBE-TIMER"); } catch (e) { snPlant("PROBE-TIMER", e); }
    try {
      window.webkit.messageHandlers.snebook.postMessage("speak:" + a + b + c);
    } catch (e) { snPlant("POST-FAILED", e); }
  }, 5000);
});
</script>
"""

chapter = xhtml("<h1>SpeakSel Probe</h1>\n" + paras + script)
nav = xhtml(
    '<nav epub:type="toc" xmlns:epub="http://www.idpf.org/2007/ops">'
    '<ol><li><a href="ch1.xhtml">Probe</a></li></ol></nav>'
)
opf = '''<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="bid">
 <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
  <dc:identifier id="bid">urn:uuid:speaksel-probe</dc:identifier>
  <dc:title>SpeakSel Probe</dc:title>
  <dc:creator>gamma probe</dc:creator>
  <dc:language>en</dc:language>
  <meta property="dcterms:modified">2026-10-31T00:00:00Z</meta>
 </metadata>
 <manifest>
  <item id="ch1" href="ch1.xhtml" media-type="application/xhtml+xml"/>
  <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
 </manifest>
 <spine><itemref idref="ch1"/></spine>
</package>
'''
container = '''<?xml version="1.0" encoding="utf-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
 <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>
'''

os.makedirs(OUT, exist_ok=True)
epub_path = os.path.join(OUT, "probe.epub")
with zipfile.ZipFile(epub_path, "w") as z:
    z.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
    z.writestr("META-INF/container.xml", container)
    z.writestr("OEBPS/content.opf", opf)
    z.writestr("OEBPS/nav.xhtml", nav)
    z.writestr("OEBPS/ch1.xhtml", chapter)

book_dir = os.path.join(
    os.environ.get("SN_BOOKS", "/tmp/sn-gamma2/xdg/speechnotes/Books"), BOOK_ID
)
os.makedirs(book_dir, exist_ok=True)
with zipfile.ZipFile(epub_path) as z:
    z.extractall(os.path.join(book_dir, "_src"))
# rebuild original.epub identically (the store wants the original archive)
with zipfile.ZipFile(os.path.join(book_dir, "original.epub"), "w") as z:
    z.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
    z.writestr("META-INF/container.xml", container)
    z.writestr("OEBPS/content.opf", opf)
    z.writestr("OEBPS/nav.xhtml", nav)
    z.writestr("OEBPS/ch1.xhtml", chapter)

manifest = {
    "id": BOOK_ID,
    "format": "epub",
    "addedAt": 815100000.0,
    "lastOpenedAt": 0.0,
    "title": "SpeakSel Probe",
    "author": "gamma probe",
    "spineCount": 1,
    "spine": ["OEBPS/ch1.xhtml"],
}
with open(os.path.join(book_dir, "manifest.json"), "w") as f:
    json.dump(manifest, f, indent=1)

print("probe epub at", epub_path)
print("book dir", book_dir)
print("needle:", SENTENCES[11])
