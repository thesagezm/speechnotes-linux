//! Split out of slideview.rs 2026-10-31 (pure code-motion).


// ---------------------------------------------------------------------------
// The injected script
// ---------------------------------------------------------------------------

pub(crate) const DECK_CSS: &str = r##"html,body{margin:0;padding:0;background:transparent;}
body{overflow-x:hidden;}
.slide{position:relative;width:calc(var(--dw) * 1pt * var(--zoom,1));height:calc(var(--dh) * 1pt * var(--zoom,1));margin:14px auto;background:#FFFFFF;box-shadow:0 2px 10px rgba(0,0,0,0.28);scroll-snap-align:center;}
.stage{position:absolute;top:0;left:0;width:calc(var(--dw) * 1pt);height:calc(var(--dh) * 1pt);transform:scale(var(--zoom,1));transform-origin:top left;overflow:hidden;}
.item{position:absolute;}
.item > svg.shape{position:absolute;top:0;left:0;width:100%;height:100%;}
.tb{position:absolute;top:0;left:0;width:100%;height:100%;box-sizing:border-box;display:flex;flex-direction:column;padding:3.6pt 7.2pt;}
.tb p{margin:0;}
.blt{margin-right:0.35em;}
.blt-img{height:0.9em;vertical-align:-0.1em;}
img.pic{display:block;width:100%;height:100%;}
.picwrap{position:absolute;top:0;left:0;width:100%;height:100%;overflow:hidden;display:block;}
.hit{position:absolute;top:0;left:0;width:100%;height:100%;}
.chart{position:absolute;top:0;left:0;width:100%;height:100%;}
.chart svg{width:100% !important;height:100% !important;}
.tblwrap{position:absolute;top:0;left:0;width:100%;height:100%;}
.tblwrap table{width:100%;height:100%;border-collapse:collapse;}
.tblwrap td{border:0.75pt solid rgba(0,0,0,0.25);vertical-align:middle;padding:2pt 5pt;overflow:hidden;}
.frame{position:absolute;top:0;left:0;width:100%;height:100%;box-sizing:border-box;display:flex;align-items:center;justify-content:center;border:1.5pt dashed rgba(0,0,0,0.35);color:rgba(0,0,0,0.55);font-family:sans-serif;}
.frame img{position:absolute;top:0;left:0;width:100%;height:100%;object-fit:contain;}
.frame-label{position:relative;background:rgba(255,255,255,0.85);padding:2pt 8pt;font-size:12pt;}
.sn-speak{background:rgba(255,196,0,0.42) !important;}
/* PowerPoint show conventions (2026-11-07): presenting is a show —
   the stage goes black around the slide, the way a projector's black bars
   read, instead of the reader's themed page background. */
body.present{overflow:hidden;background:#000000;}
html.snap{scroll-snap-type:y mandatory;overflow-y:auto;background:#000000;}
body.present .slide{width:100vw;height:100vh;margin:0;box-shadow:none;}
body.present .stage{transform:translate(calc((100vw - var(--dw) * 1pt * var(--pz,1))/2),calc((100vh - var(--dh) * 1pt * var(--pz,1))/2)) scale(var(--pz,1));}
"##;

/// Speak-from-selection: parse the page's
/// `<slide>:<byte>` selection report; `-1:-1` (a selection over
/// non-speakable content) is `None`.
pub(crate) fn parse_sel_report(rest: &str) -> Option<(usize, usize)> {
    let (slide, byte) = rest.trim().split_once(':')?;
    let (slide, byte) = (slide.parse::<i64>().ok()?, byte.parse::<i64>().ok()?);
    if slide < 0 || byte < 0 {
        return None;
    }
    Some((slide as usize, byte as usize))
}

/// The slide script: `window.__snslides` — show, current, fit, present,
/// highlight — plus the scroll reporting and the presentation input.
pub(crate) const SLIDES_SCRIPT: &str = r##"(function () {
    if (window.__snslides) { return; }
    var cfg = { zoom: 1, fit: "width", present: false, clickAdvance: true, manual: 1, ds: 1 };

    //  the fit is computed here, in the page, because the page's CSS
    // viewport is the GTK allocation divided by the web process's device
    // scale — a zoom computed outside laid the slide out wider than this
    // viewport and cut it at the right edge on a scaled desktop. The GTK
    // size pushed with every setFit derives the scale, so a manual zoom
    // (a GTK-pixel factor from the zoom buttons) still lands at the size
    // those buttons promise.
    function fitZoom(mode, vw, vh, manual) {
        var de = document.documentElement;
        var dw = +de.dataset.w || 720, dh = +de.dataset.h || 405;
        var ds = (vw > 0 && window.innerWidth > 0) ? vw / window.innerWidth : 1;
        ds = Math.max(0.2, Math.min(ds, 8));
        cfg.ds = ds;
        if (mode === 'none') { return Math.max(0.05, Math.min(manual / ds, 8)); }
        var availW = Math.max(window.innerWidth - 28, 120);
        var byW = availW / (dw * 4 / 3);
        if (mode === 'page') {
            var availH = Math.max(window.innerHeight - 28, 90);
            return Math.max(Math.min(byW, availH / (dh * 4 / 3)), 0.05);
        }
        return Math.max(Math.min(byW, 8.0), 0.05);
    }

    function post(msg) {
        try { window.webkit.messageHandlers.snslides.postMessage(msg); } catch (e) {}
    }

    function slides() { return document.querySelectorAll('.slide'); }

    function nearest() {
        var best = 0, dist = Infinity;
        var els = slides();
        for (var i = 0; i < els.length; i++) {
            var r = els[i].getBoundingClientRect();
            var d = Math.abs(r.top + r.height / 2 - window.innerHeight / 2);
            if (d < dist) { dist = d; best = i; }
        }
        return best;
    }

    var reported = -1;
    function report(i) {
        if (i === reported) { return; }
        reported = i;
        post('slidechanged:' + i);
    }

    function show(i) {
        var el = document.getElementById('s' + i);
        if (!el) { return; }
        el.scrollIntoView({ behavior: 'auto', block: 'start' });
        lastShow = Date.now();
        report(i);
    }

    // The corrective re-land: the same scroll, but it does NOT arm the
    // commanded-show window — a re-fit burst schedules a re-land per frame,
    // and each must stay free to correct the next frame's drift.
    function land(i) {
        var el = document.getElementById('s' + i);
        if (!el) { return; }
        el.scrollIntoView({ behavior: 'auto', block: 'start' });
        report(i);
    }

    function step(delta) {
        var els = slides();
        var i = nearest() + delta;
        if (i < 0 || i >= els.length) { return; }
        els[i].scrollIntoView({ behavior: 'auto', block: 'start' });
        report(i);
    }

    function presentZoom() {
        var de = document.documentElement;
        var dw = +de.dataset.w || 720, dh = +de.dataset.h || 405;
        var z = Math.min(window.innerWidth / (dw * 4 / 3), window.innerHeight / (dh * 4 / 3));
        de.style.setProperty('--pz', z);
    }

    // The resize drift (2026-11-13): a re-fit re-zooms the deck, so
    // EVERY slide's box changes height while the browser keeps scrollTop —
    // the view lands on a different slide, and the settled scroll report
    // drags the reader (and the saved position) with it. `reland` re-lands
    // on the slide the reader stood on once the new layout has settled, and
    // `relayout` holds the scroll reports off while the correction lands so
    // the transient position never steers anything.
    var relayout = 0;
    var lastShow = 0;
    function reland(i) {
        if (i < 0) { return; }
        relayout++;
        requestAnimationFrame(function () {
            requestAnimationFrame(function () {
                // A show commanded within the settle window owns the
                // position: the fit's re-land must not fight it (the load
                // flush shows the queued slide and the fit's re-land stands
                // down). A re-land's own scroll arms nothing.
                if (Date.now() - lastShow > 500) { land(i); }
                setTimeout(function () { relayout--; }, 200);
            });
        });
    }

    var scrollTimer = null;
    window.addEventListener('scroll', function () {
        if (relayout) { return; }
        if (scrollTimer) { return; }
        scrollTimer = setTimeout(function () {
            scrollTimer = null;
            report(nearest());
        }, 150);
    }, true);

    document.addEventListener('keydown', function (ev) {
        if (!cfg.present) { return; }
        if (ev.key === 'ArrowRight' || ev.key === 'ArrowDown' || ev.key === ' ' ||
            ev.key === 'PageDown' || ev.key === 'Enter') {
            step(1); ev.preventDefault();
        } else if (ev.key === 'ArrowLeft' || ev.key === 'ArrowUp' || ev.key === 'PageUp') {
            step(-1); ev.preventDefault();
        } else if (ev.key === 'Escape') {
            // PowerPoint show conventions (2026-11-07): Esc ends the
            // show. Reported to the app, which leaves fill-the-window — the
            // page cannot reach the window's chrome from here.
            snPost('esc:0:0'); ev.preventDefault();
        }
    });

    document.addEventListener('click', function () {
        if (cfg.present && cfg.clickAdvance) { step(1); }
    });

    window.addEventListener('resize', function () {
        if (cfg.present) { presentZoom(); }
    });

    // Speak-from-selection: a selection over a
    // speakable element shows the pill at its head; the deck's own data-s
    // ranges give the exact byte, so no search is needed. Every settled
    // selection is reported too, so the app's F5 can use it without the
    // pill.
    var snBubble = null;
    var snSel = null;
    function snPost(msg) {
        try { window.webkit.messageHandlers.snslides.postMessage(msg); } catch (e) {}
    }
    function snHideBubble() { if (snBubble) { snBubble.remove(); snBubble = null; } }
    function snShowBubble(cx, cy) {
        snHideBubble();
        snBubble = document.createElement("div");
        snBubble.textContent = "Speak from here";
        snBubble.style.cssText = "position:fixed;z-index:2147483647;" +
            "background:rgba(28,28,30,0.85);color:#fff;font:13px sans-serif;" +
            "padding:4px 10px;border-radius:12px;cursor:pointer;" +
            "user-select:none;-webkit-user-select:none;" +
            "box-shadow:0 1px 4px rgba(0,0,0,0.35);";
        snBubble.style.left = Math.max(4, cx - 60) + "px";
        snBubble.style.top = Math.max(4, cy - 34) + "px";
        snBubble.addEventListener("mousedown", function (ev) {
            ev.preventDefault(); ev.stopPropagation();
            if (snSel) { snPost("speak:" + snSel.slide + ":" + snSel.byte); }
            snHideBubble();
        });
        (document.body || document.documentElement).appendChild(snBubble);
    }
    function snSlideOf(node) {
        while (node) {
            if (node.classList && node.classList.contains("slide")) { return +node.id.slice(1); }
            node = node.parentNode;
        }
        return -1;
    }
    function snStartByte(range) {
        var els = document.querySelectorAll("[data-s]");
        var start = -1;
        for (var i = 0; i < els.length; i++) {
            if (range.intersectsNode && range.intersectsNode(els[i])) {
                var ds = +els[i].getAttribute("data-s");
                if (start < 0 || ds < start) { start = ds; }
            }
        }
        return start;
    }
    var snSelTimer = null;
    document.addEventListener("selectionchange", function () {
        if (snSelTimer) { clearTimeout(snSelTimer); }
        snSelTimer = setTimeout(function () {
            snSelTimer = null;
            var sel = window.getSelection && window.getSelection();
            if (!sel || sel.isCollapsed || sel.rangeCount < 1) {
                snSel = null; snHideBubble(); snPost("sel:-1:-1"); return;
            }
            var range = sel.getRangeAt(0);
            var slide = snSlideOf(range.startContainer);
            if (slide < 0) { slide = snSlideOf(range.endContainer); }
            var byte = snStartByte(range);
            if (slide < 0 || byte < 0) {
                snSel = null; snHideBubble(); snPost("sel:-1:-1"); return;
            }
            snSel = { slide: slide, byte: byte };
            snPost("sel:" + slide + ":" + byte);
            var r = range.getBoundingClientRect();
            snShowBubble(r.left + r.width / 2, r.top);
        }, 300);
    });

    window.__snslides = {
        show: show,
        current: nearest,
        // `idx` is the app's own belief about the slide on screen — the one
        // position that does not drift when the re-zoom moves every box.
        // Reading the page's own nearest() here (as this once did) re-reads
        // a position the PREVIOUS setFit of the same resize burst had
        // already drifted, and the error compounds frame by frame.
        setFit: function (mode, vw, vh, manual, idx) {
            cfg.fit = mode; cfg.manual = +manual || 1;
            var cur = cfg.present ? -1
                : (typeof idx === 'number' && idx >= 0 ? idx : nearest());
            cfg.zoom = fitZoom(mode, vw, vh, cfg.manual);
            if (!cfg.present) {
                document.documentElement.style.setProperty('--zoom', cfg.zoom);
                reland(cur);
            }
        },
        // `idx` is the app's own belief about the slide on screen. Entering
        // present re-lays the whole page out (fullscreen, black surround),
        // and a `nearest()` read mid-resize has been measured to land the
        // show on the wrong slide — the commanded index is the truth.
        setPresent: function (on, click, idx) {
            cfg.present = !!on; cfg.clickAdvance = !!click;
            document.body.classList.toggle('present', !!on);
            document.documentElement.classList.toggle('snap', !!on);
            if (on) {
                presentZoom();
                reland(typeof idx === 'number' && idx >= 0 ? idx : nearest());
            }
            else { document.documentElement.style.setProperty('--zoom', cfg.zoom); }
        },
        highlight: function (s, e) {
            clearHighlight();
            var els = document.querySelectorAll('[data-s]'), first = null;
            for (var i = 0; i < els.length; i++) {
                var el = els[i];
                var ds = +el.getAttribute('data-s'), de = +el.getAttribute('data-e');
                if (ds < e && de > s) {
                    el.classList.add('sn-speak');
                    if (!first) { first = el; }
                }
            }
            if (first) { first.scrollIntoView({ behavior: 'smooth', block: 'center' }); }
        },
        clearHighlight: clearHighlight
    };

    function clearHighlight() {
        var els = document.querySelectorAll('.sn-speak');
        for (var i = 0; i < els.length; i++) { els[i].classList.remove('sn-speak'); }
    }
})();
"##;
