// The book shell: the script the reader's webview boots. It receives the
// open instruction (book URL, format, resume target) from the query string,
// dispatches the format — the vendored foliate modules for the container
// formats, the in-memory book builders for the headless ones — and bridges
// the view's events to the Rust reader through the script-message channel.
//
// The dispatch mirrors readest's DocumentLoader.open(): a .txt goes through
// the chapteriser, .html/.htmlz through the HTML book builder, .rtf through
// the RTF renderer, .fb2/.fbz through fb2.js, the Mobipocket/Kindle family
// through mobi.js (its JS decompressors are the reason the letters stop
// repeating), and a zip that is none of those is an EPUB.

import { View } from './view.js'
import { txtToChapters } from './sn-txt.js'
import { buildDocBook } from './sn-docbook.js'
import { htmlToBook, htmlzToBook } from './sn-html.js'
import { rtfToBook } from './sn-rtf.js'

// The boot's milestones go to the console — SN_BOOKVIEW_CONSOLE=1 sends
// them to stdout, which is how a white pane gets diagnosed from outside.
const say = (...parts) => { try { console.log('[sn-shell]', ...parts) } catch {} }
say('module loaded')

const CHANNEL = 'snbook'

const post = msg => {
    try { window.webkit.messageHandlers[CHANNEL].postMessage(msg) } catch (e) {}
}

const params = new URLSearchParams(location.search)
const bookUrl = params.get('src')
const format = (params.get('format') ?? 'epub').toLowerCase()
const resumeCfi = params.get('cfi') ?? ''

const error = message => post(`error:${message}`)

// -- format dispatch ---------------------------------------------------------

const zipLoader = async bytes => {
    const { configure, ZipReader, BlobReader, TextWriter, BlobWriter } =
        await import('./vendor/zip.js')
    configure({ useWebWorkers: false })
    const reader = new ZipReader(new BlobReader(new Blob([bytes])))
    const entries = await reader.getEntries()
    const map = new Map(entries.map(entry => [entry.filename, entry]))
    const load = f => (name, ...args) =>
        map.has(name) ? f(map.get(name), ...args) : null
    return {
        entries,
        loadText: load(entry => entry.getData(new TextWriter())),
        loadBlob: load((entry, type) => entry.getData(new BlobWriter(type))),
        getSize: name => map.get(name)?.uncompressedSize ?? 0,
    }
}

const openZipBook = async bytes => {
    // The dispatch readest's DocumentLoader.open() runs on a zip: a comic
    // archive, a zipped FB2, else an EPUB.
    const loader = await zipLoader(bytes)
    const name = params.get('format') // the manifest's format, authoritative
    const isCbz = name === 'cbz'
    const isFbz = name === 'fbz' || name === 'fb2.zip' ||
        loader.entries.some(e => e.filename.toLowerCase().endsWith('.fb2'))
    if (isFbz) {
        const entry = loader.entries.find(e => e.filename.toLowerCase().endsWith('.fb2'))
        const blob = await loader.loadBlob((entry ?? loader.entries[0]).filename)
        const { makeFB2 } = await import('./fb2.js')
        return makeFB2(blob)
    }
    if (isCbz) {
        const { makeComicBook } = await import('./comic-book.js')
        return makeComicBook(loader, new Blob([bytes]))
    }
    const { EPUB } = await import('./epub.js')
    return new EPUB(loader).init()
}

const openMobi = async bytes => {
    const { isMOBI, MOBI } = await import('./mobi.js')
    const file = new Blob([bytes])
    if (!await isMOBI(file))
        throw new Error('not a Mobipocket book — the .pdb extension is shared ' +
            'with other Palm-era formats this reader cannot open')
    const fflate = await import('./vendor/fflate.js')
    return new MOBI({ unzlib: fflate.unzlibSync }).open(file)
}

const openFb2 = async bytes => {
    const { makeFB2 } = await import('./fb2.js')
    return makeFB2(new Blob([bytes]))
}

const openTxt = async bytes => {
    const { chapters, language } = txtToChapters(bytes)
    if (!chapters.length) throw new Error('the file has no readable text')
    return buildDocBook(chapters, chapters
        .filter(c => c.title)
        .map(c => ({
            label: c.title,
            index: chapters.indexOf(c),
            anchorId: null,
            subitems: [],
        })), {
        title: params.get('title') ?? '',
        author: params.get('author') ?? '',
        language,
    })
}

const openHtml = bytes => htmlToBook(bytes, {
    title: params.get('title') ?? '',
    author: params.get('author') ?? '',
})

const openHtmlz = async bytes => {
    const zipMod = await import('./vendor/zip.js')
    return htmlzToBook(bytes, zipMod, {
        title: params.get('title') ?? '',
        author: params.get('author') ?? '',
    })
}

const openRtf = bytes => rtfToBook(bytes, {
    title: params.get('title') ?? '',
    author: params.get('author') ?? '',
})

const openFile = async bytes => {
    switch (format) {
        case 'txt': case 'text': return openTxt(bytes)
        case 'rtf': return openRtf(bytes)
        case 'html': case 'htm': return openHtml(bytes)
        case 'htmlz': return openHtmlz(bytes)
        case 'fb2': return openFb2(bytes)
        case 'fbz': case 'fb2.zip': return openZipBook(bytes)
        case 'mobi': case 'azw': case 'azw3': case 'prc': case 'pdb':
            return openMobi(bytes)
        case 'epub': case 'cbz': return openZipBook(bytes)
        default:
            throw new Error(`this reader cannot open .${format} files`)
    }
}

// -- the view ----------------------------------------------------------------

let view = null
let appearance = { respectStyles: true, dark: false }

// A loaded section document: strip the scripts and script-bearing attributes
// the book carried (the paginator's iframe sandbox allows scripts because of
// a WebKit bug that renders the sandbox useless without it), and apply the
// reader's type layer — the same rules the old per-page script had, gated on
// respect-styles so a respected book keeps its own design. The scroll and
// selection listeners attach to the section document itself: neither event
// crosses the iframe boundary, so a top-document listener never hears them.
const loadedTexts = new Map()
const onSectionLoad = ({ doc, index }) => {
    for (const script of Array.from(doc.querySelectorAll('script'))) script.remove()
    for (const el of Array.from(doc.querySelectorAll('*'))) {
        for (const attr of Array.from(el.attributes ?? [])) {
            if (/^on/i.test(attr.name)) el.removeAttribute(attr.name)
        }
    }
    applyTypeLayer(doc)

    // The section's own prose, cached and reported once it lands: the voice
    // and the read-along view read exactly what the page shows — the
    // engine's own decompression included, which is the point. The cache
    // lets Rust re-read a section's text after a late relocation into it.
    try {
        const text = doc.body?.textContent ?? ''
        loadedTexts.set(index, text)
        post(`text:${index}:${JSON.stringify(text)}`)
    } catch (e) { /* a section without a body reports nothing */ }

    const win = doc.defaultView
    if (win) {
        let scrollTimer = null
        win.addEventListener('scroll', () => {
            if (scrollTimer) return
            scrollTimer = setTimeout(() => {
                scrollTimer = null
                reportScroll()
            }, 120)
        }, { passive: true })
        let selTimer = null
        doc.addEventListener('selectionchange', () => {
            if (selTimer) { clearTimeout(selTimer); selTimer = null }
            selTimer = setTimeout(() => {
                selTimer = null
                const sel = win.getSelection?.()
                selText = sel && !sel.isCollapsed ? String(sel) : ''
                post(`sel:${selText}`)
                if (!selText) { hideBubble(); return }
                try {
                    const r = sel.getRangeAt(0).getBoundingClientRect()
                    showBubble(r.left + r.width / 2, r.top)
                } catch { hideBubble() }
            }, 300)
        })
    }
}

// Where the visible content starts inside its section's own text — the
// offset the voice needs to start reading *from the page on screen* rather
// than from the section's top. Counted the way `loadedTexts` is built:
// text nodes in document order, so the offset indexes exactly the string
// the text report posts (UTF-16 units, the bookmark offset's unit).
const textOffsetOf = range => {
    try {
        const start = range?.startContainer
        const doc = start?.ownerDocument
        if (!start || !doc?.body) return null
        const walker = doc.createTreeWalker(doc.body, NodeFilter.SHOW_TEXT)
        let off = 0, cur
        while ((cur = walker.nextNode())) {
            if (cur === start) return off + (range.startOffset ?? 0)
            const pos = cur.compareDocumentPosition(start)
            // The start point follows this text node: keep counting. It
            // precedes it (an element container's first text): this node's
            // start is the offset.
            if (pos & Node.DOCUMENT_POSITION_FOLLOWING) { off += cur.data.length; continue }
            return off
        }
    } catch (e) { /* a detached range has no position worth reporting */ }
    return null
}

// The last relocation's page start, for the on-demand `pageOffset()` query —
// the relocate post carries the same numbers, so Rust can read it either way.
let lastPage = { index: -1, offset: null }

const applyTypeLayer = doc => {
    const de = doc.documentElement
    if (appearance.respectStyles) {
        de.style.removeProperty('font-size')
        de.style.removeProperty('letter-spacing')
        de.style.removeProperty('color')
        de.style.removeProperty('background-color')
        const body = doc.body
        if (body) {
            body.style.removeProperty('color')
            body.style.removeProperty('background-color')
        }
    } else {
        if (appearance.fontSize) de.style.fontSize = `${appearance.fontSize}px`
        if (appearance.letterSpacing)
            de.style.letterSpacing = `${appearance.letterSpacing}px`
        const styleId = 'sn-viewer-style'
        let ss = doc.getElementById(styleId)
        if (!ss) {
            ss = doc.createElement('style')
            ss.id = styleId
            ;(doc.head ?? de).appendChild(ss)
        }
        const css = []
        if (appearance.fontFamily)
            css.push(`body{font-family:${JSON.stringify(appearance.fontFamily)} !important;}`)
        if (appearance.lineHeight)
            css.push(`body{line-height:${appearance.lineHeight} !important;}`)
        if (appearance.paragraphSpacing)
            css.push(`body p, body li, body div{margin-bottom:${appearance.paragraphSpacing}px !important;}`)
        if (appearance.background && appearance.foreground) {
            de.style.setProperty('--sn-bg', appearance.background)
            de.style.setProperty('--sn-fg', appearance.foreground)
            css.push(`html{background-color:var(--sn-bg) !important;}`)
            css.push(`body{color:var(--sn-fg) !important;background-color:transparent !important;}`)
            if (appearance.link)
                css.push(`:link, :link *{color:var(--sn-link) !important;}`)
        }
        ss.textContent = css.join('\n')
    }
}

// Chapter titles arrive XML-escaped (they were written into XHTML); the
// sidebar renders plain text, so the five entities come back out here.
const unescapeXml = str => str
    .replace(/&(amp|lt|gt|quot|#39);/g, (_, ent) => ({
        amp: '&', lt: '<', gt: '>', quot: '"', '#39': "'",
    }[ent]))

// Each contents row carries the section it lands on, resolved through the
// book's own href table — the sidebar's activation and the open-row mark
// both key on it. mobi.js resolves asynchronously (KF8 sections decompress
// on demand); await normalises sync and async resolvers alike.
const flattenToc = async items => Promise.all(items.map(async item => ({
    label: unescapeXml(item.label ?? ''),
    href: item.href ?? '',
    index: await resolveIndex(item.href ?? ''),
    subitems: item.subitems?.length ? await flattenToc(item.subitems) : [],
})))

const resolveIndex = async href => {
    if (!view?.book) return 0
    try {
        const resolved = await view.book.resolveHref(href)
        return resolved?.index ?? 0
    } catch {
        return 0
    }
}

const boot = async () => {
    try {
        say('boot', bookUrl, 'format', format)
        const res = await fetch(bookUrl)
        if (!res.ok) throw new Error(`the book's bytes could not be read (${res.status})`)
        const bytes = new Uint8Array(await res.arrayBuffer())
        say('fetched', bytes.length, 'bytes')

        const book = await openFile(bytes)
        say('book built', book?.sections?.length, 'sections')

        // The custom element upgrades synchronously: view.js ran its
        // customElements.define before this module body executed.
        view = document.createElement('foliate-view')
        document.body.append(view)

        view.addEventListener('load', e => onSectionLoad(e.detail))
        view.addEventListener('relocate', e => {
            const { cfi, index, fraction, tocItem, range } = e.detail
            const label = tocItem?.label ?? ''
            const textOffset = textOffsetOf(range)
            lastPage = { index: index ?? 0, offset: textOffset }
            post(`relocate:${JSON.stringify({
                cfi: cfi ?? '',
                index: index ?? 0,
                fraction: fraction ?? 0,
                label,
                textOffset,
                sectionCount: view.book.sections.length,
            })}`)
        })
        view.addEventListener('external-link', e => {
            e.preventDefault()
            post(`external:${e.detail.href}`)
            return false
        })

        // The boot's slowest steps are marked, so a white pane can be read
        // from the console: the last marker is where it stopped.
        say('opening the book')
        await view.open(book)
        say('book opened, laying out')

        // The paginator's knobs the view exposes as attributes: no continuous
        // scroll (each section scrolls its own iframe — the scroll evidence
        // the reader's keys rely on), and the paginator's own page background
        // stands down so a respected book paints its own paper.
        if (view.renderer) {
            view.renderer.setAttribute('no-continuous-scroll', '')
        }

        // The book report: metadata and the book's own nested contents —
        // the TOC the sidebar renders (real chapters, real depths, straight
        // from the format's structure).
        say('resolving the contents')
        const toc = await flattenToc(book.toc ?? [])
        say('contents resolved, reporting the book')
        post(`book:${JSON.stringify({
            title: typeof book.metadata?.title === 'string'
                ? book.metadata.title : '',
            author: typeof book.metadata?.author === 'string'
                ? book.metadata.author
                : Array.isArray(book.metadata?.author)
                    ? book.metadata.author.map(a =>
                        typeof a === 'string' ? a : a?.name ?? '').join(', ')
                    : '',
            language: book.metadata?.language ?? '',
            sectionCount: book.sections.length,
            toc,
        })}`)

        // Resume: the saved CFI when there is one, else the book's own text
        // start (its bodymatter landmark, or the first linear section).
        say('initialising the view')
        await view.init({
            lastLocation: resumeCfi || undefined,
            showTextStart: !resumeCfi,
        })
        say('view ready')
        booted = true
        post('ready')
    } catch (e) {
        error(String(e?.message ?? e))
    }
}

// A boot that never completes must still reach Rust: 45 s after the module
// loaded, if no 'ready' was posted, say so through the channel — a white
// pane with a name beats a white pane alone. (boot() flips the flag on its
// way out; the timer posts nothing when the book came up.)
let booted = false
window.addEventListener('load', () => {
    setTimeout(() => {
        if (!booted) post('error:the book engine did not finish ' +
            'loading (no ready within 45 s)')
    }, 45000)
})

// -- the Rust-callable surface -----------------------------------------------

const primaryDoc = () =>
    view?.renderer?.getContents?.()
        .find(x => x.index === view.renderer.primaryIndex)?.doc

window.__snbook = {
    apply(next) {
        appearance = { ...appearance, ...next }
        for (const { doc } of view.renderer.getContents()) applyTypeLayer(doc)
        // The shell's own ground follows the scheme in dark modes so page
        // turns never flash white.
        document.documentElement.classList.toggle('sn-dark', !!appearance.dark)
        document.documentElement.style.setProperty(
            '--sn-bg', appearance.background ?? '#ffffff')
    },
    async goTo(target) { await view?.goTo(target) },
    async goToIndex(index) { await view?.goTo(index) },
    async goToHref(href) { await view?.goTo(href) },
    async goToFraction(fraction) { await view?.goToFraction(fraction) },
    async next() { await view?.next() },
    async prev() { await view?.prev() },
    async scrollBy(direction) {
        // The paginator's scrolled mode lays sections into their own iframe
        // documents; the scroll happens there, the relocate follows.
        const doc = primaryDoc()
        if (!doc) return
        const win = doc.defaultView
        win.scrollBy({ top: direction * 0.12 * (win.innerHeight || 600),
            behavior: 'auto' })
    },
    selection: () => {
        const doc = primaryDoc()
        const sel = doc?.defaultView?.getSelection?.()
        return sel && !sel.isCollapsed ? String(sel) : ''
    },
    sectionText: index => {
        // Whether the section has loaded, with its prose: an absent key and
        // a loaded-but-blank section are different answers — the walk must
        // not wait on a report that already fired and was dropped, nor pass
        // a section that is still loading.
        return JSON.stringify({
            loaded: loadedTexts.has(index),
            text: loadedTexts.get(index) ?? '',
        })
    },
    // Where the visible page starts in its section's text, as the last
    // relocation reported it: `{index, offset}` (UTF-16 units), `offset`
    // null when the position carries no computable text start.
    pageOffset: () => lastPage,
}

// The scroll evidence the reader's Up/Down keys need: where the primary
// section's document stands, `scroll:<y>:<max>` — the protocol the old
// per-page script spoke. Fired by the per-document scroll listener that
// onSectionLoad attaches.
const reportScroll = () => {
    const doc = primaryDoc()
    const win = doc?.defaultView
    if (!win) return
    const max = Math.max(0, (doc.documentElement.scrollHeight || 0) -
        (win.innerHeight || 0))
    post(`scroll:${win.scrollY || 0}:${max}`)
}

// The selection bubble — "Speak from here" — carried over from the old
// viewer so the speak-from-selection flow keeps working. It lives in the
// top document but is raised by the per-document selectionchange listener.
let bubble = null
let selText = ''
const hideBubble = () => { if (bubble) { bubble.remove(); bubble = null } }
const showBubble = (cx, cy) => {
    hideBubble()
    bubble = document.createElement('div')
    bubble.textContent = 'Speak from here'
    bubble.style.cssText = 'position:fixed;z-index:2147483647;' +
        'background:rgba(28,28,30,0.85);color:#fff;font:13px sans-serif;' +
        'padding:4px 10px;border-radius:12px;cursor:pointer;' +
        'user-select:none;-webkit-user-select:none;' +
        'box-shadow:0 1px 4px rgba(0,0,0,0.35);'
    bubble.style.left = Math.max(4, cx - 60) + 'px'
    bubble.style.top = Math.max(4, cy - 34) + 'px'
    bubble.addEventListener('mousedown', ev => {
        ev.preventDefault(); ev.stopPropagation()
        post(`speak:${selText}`)
        hideBubble()
    })
    document.body.appendChild(bubble)
}

boot()
