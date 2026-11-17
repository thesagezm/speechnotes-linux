//! The zoomable viewer behind a click on a note image.
//!
//! Both phone builds open a note's picture on its own surface — iOS's
//! `ZoomableImageView` (1–5×, double-tap to 2.5×) and Android's
//! `ZoomableImageViewer` (pinch and pan, close on a tap). Parity here is a
//! `ScrolledWindow` whose child is a `Picture` sized by the zoom: the
//! scrollbars are the pan, and zooming is a resize of the child, so the
//! toolkit's own scrolling does the dragging, the clamping and the
//! scrollbars' own show/hide.
//!
//! The size is applied through `set_size_request` because a viewport
//! allocates its child `max(viewport, child minimum)` — the request *is* the
//! zoomed content size, and the scrollable area falls out of it.
//!
//! One trap is priced in: the window is **not** re-fitted when it is
//! resized. Refitting inside the allocation cycle feeds the scrollbars' own
//! resize back into the content size, which is a layout loop. "Fit" is a
//! button; the user asks for it.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
//  the file body calls `gtk::…` throughout but its import block
// lost the alias line; added to match the other modules.
use gtk4 as gtk;

/// The zoom range, as a multiple of the fit size. iOS's viewer uses the same
/// 1–5×; below 1× (smaller than fit) there is nothing to see that fit does
/// not already show.
const MIN_ZOOM: f64 = 1.0;
const MAX_ZOOM: f64 = 5.0;
/// One toolbar step or wheel notch.
const ZOOM_STEP: f64 = 1.25;
/// iOS's double-tap zoom: 2.5× on the second tap.
const DOUBLE_TAP_ZOOM: f64 = 2.5;
/// Zooms within this of 1.0 count as "at fit", so a double-tap toggles
/// cleanly instead of fighting rounding residue.
const AT_FIT_EPSILON: f64 = 0.01;

/// The viewer's mutable state, shared by the toolbar, the gestures and the
/// initial-fit poll; they all change the same numbers and all apply through
/// [`apply_zoom`].
struct Zoom {
    image_w: i64,
    image_h: i64,
    /// The scale that fits the picture in the viewport, computed once after
    /// the first allocation. The zoom is a multiple of this, so "fit" is
    /// always zoom 1.0 no matter the window size.
    fit: f64,
    zoom: f64,
    /// Whether `fit` has been measured. Before the first allocation the
    /// viewport's page size is 0 and any fit computed from it is garbage.
    fit_known: bool,
}

/// Open `path` in a zoomable viewer over `parent`, titled with `alt`.
///
/// The viewer loads and owns its texture; callers hand over a path, which is
/// what every image surface in this app already has. A file that cannot be
/// decoded is logged and dropped — the click came from a picture that *was*
/// decoded, so this is a race with a deletion, not a path worth a dialog.
pub fn open(parent: Option<&gtk::Window>, path: &Path, alt: &str) {
    let texture = match gtk::gdk::Texture::from_file(&gtk::gio::File::for_path(path)) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "the image viewer could not decode the picture");
            return;
        }
    };
    let state = Rc::new(RefCell::new(Zoom {
        image_w: texture.intrinsic_width() as i64,
        image_h: texture.intrinsic_height() as i64,
        fit: 1.0,
        zoom: 1.0,
        fit_known: false,
    }));

    let picture = gtk::Picture::builder()
        .paintable(&texture)
        .content_fit(gtk::ContentFit::Contain)
        .can_shrink(true)
        .build();

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_width(false)
        .propagate_natural_height(false)
        .child(&picture)
        .build();
    scroll.set_vexpand(true);

    // The bottom toolbar: zoom out / fit / actual size / zoom in. Words, not
    // icons, because two of the four ("Fit", "1:1") are words anyway and a
    // half-icon half-word row reads as unfinished.
    let bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk::Align::Center)
        .build();
    bar.add_css_class("image-viewer-bar");
    let zoom_out = gtk::Button::with_label("-");
    let fit_btn = gtk::Button::with_label("Fit");
    let one_to_one = gtk::Button::with_label("1:1");
    let zoom_in = gtk::Button::with_label("+");
    for b in [&zoom_out, &fit_btn, &one_to_one, &zoom_in] {
        b.add_css_class("flat");
        bar.append(b);
    }

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&scroll);
    column.append(&bar);

    // About 90% of the parent, so the note stays visible around the viewer
    // the way a sheet does on the phones.
    let (w, h) = parent
        .map(|p| {
            let (w, h) = p.default_size();
            (w * 9 / 10, h * 9 / 10)
        })
        .unwrap_or((1000, 700));
    //  gtk4 0.9's WindowBuilder has no `default_size`; the original
    // builder call is set on the window after construction instead.
    let window = gtk::Window::builder()
        .title(if alt.is_empty() { "Image" } else { alt })
        .destroy_with_parent(true)
        .child(&column)
        .build();
    window.set_default_size(w.max(300), h.max(200));
    if let Some(p) = parent {
        window.set_transient_for(Some(p));
    }
    window.add_css_class("image-viewer");

    // Esc closes: the phone builds dismiss on a tap, a keyboard desktop
    // expects the key.
    let esc = gtk::EventControllerKey::new();
    let esc_window = window.clone();
    esc.connect_key_pressed(move |_, key, _, _| {
        if key == gtk4::gdk::Key::Escape {
            esc_window.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(esc);

    // Ctrl+wheel zooms on the pointer's column; a bare wheel stays a scroll,
    // which is the pan.
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let state = state.clone();
        let picture = picture.clone();
        let scroll = scroll.clone();
        wheel.connect_scroll(move |controller, _dx, dy| {
            if !controller
                .current_event_state()
                .contains(gtk4::gdk::ModifierType::CONTROL_MASK)
            {
                return glib::Propagation::Proceed;
            }
            let factor = if dy < 0.0 { ZOOM_STEP } else { 1.0 / ZOOM_STEP };
            let next = state.borrow().zoom * factor;
            apply_zoom(&state, &picture, &scroll, next);
            glib::Propagation::Stop
        });
    }
    scroll.add_controller(wheel);

    // Double-click toggles fit and iOS's 2.5×, toward the viewport centre.
    let tap = gtk::GestureClick::new();
    {
        let state = state.clone();
        let picture = picture.clone();
        let scroll = scroll.clone();
        tap.connect_pressed(move |_, n_press, _, _| {
            if n_press != 2 {
                return;
            }
            let at_fit = state.borrow().zoom <= 1.0 + AT_FIT_EPSILON;
            apply_zoom(
                &state,
                &picture,
                &scroll,
                if at_fit { DOUBLE_TAP_ZOOM } else { 1.0 },
            );
        });
    }
    picture.add_controller(tap);

    {
        let state = state.clone();
        let picture = picture.clone();
        let scroll = scroll.clone();
        zoom_out.connect_clicked(move |_| {
            let next = state.borrow().zoom / ZOOM_STEP;
            apply_zoom(&state, &picture, &scroll, next);
        });
    }
    {
        let state = state.clone();
        let picture = picture.clone();
        let scroll = scroll.clone();
        zoom_in.connect_clicked(move |_| {
            let next = state.borrow().zoom * ZOOM_STEP;
            apply_zoom(&state, &picture, &scroll, next);
        });
    }
    {
        let state = state.clone();
        let picture = picture.clone();
        let scroll = scroll.clone();
        fit_btn.connect_clicked(move |_| apply_zoom(&state, &picture, &scroll, 1.0));
    }
    {
        let state = state.clone();
        let picture = picture.clone();
        let scroll = scroll.clone();
        // "1:1" is one picture pixel per screen pixel: a zoom of 1/fit. A
        // picture smaller than the viewport has fit > 1 and no zoom below
        // fit is reachable (MIN_ZOOM), which is the same cap iOS carries.
        one_to_one.connect_clicked(move |_| {
            let next = 1.0 / state.borrow().fit;
            apply_zoom(&state, &picture, &scroll, next);
        });
    }

    // Measure the fit once the viewport has its real size: at present time
    // the allocation has not happened and the page sizes read 0. A bounded
    // retry, because a window the WM has not mapped yet stays at 0 for a
    // few frames.
    {
        let state = state.clone();
        let picture = picture.clone();
        let scroll = scroll.clone();
        let window = window.clone();
        let tries = RefCell::new(0i32);
        glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
            let seen = {
                let mut t = tries.borrow_mut();
                *t += 1;
                *t
            };
            if !window.is_visible() || seen > 100 {
                return glib::ControlFlow::Break;
            }
            let vw = scroll.hadjustment().page_size();
            let vh = scroll.vadjustment().page_size();
            if vw <= 0.0 || vh <= 0.0 {
                return glib::ControlFlow::Continue;
            }
            let mut z = state.borrow_mut();
            z.fit = fit_scale(z.image_w, z.image_h, vw, vh);
            z.zoom = 1.0;
            z.fit_known = true;
            let (w, h) = zoomed_size(z.image_w, z.image_h, z.fit, z.zoom);
            picture.set_size_request(w as i32, h as i32);
            glib::ControlFlow::Break
        });
    }

    window.present();
}

/// Change the zoom, keeping the content under the viewport's centre still.
///
/// The anchor is read from the *old* adjustments before the child is resized
/// and written back one idle later, after the queued allocation has updated
/// the adjustments' uppers: `set_value` clamps against the current upper, so
/// writing immediately would clamp a zoom-in to the old content's edge.
fn apply_zoom(
    state: &Rc<RefCell<Zoom>>,
    picture: &gtk::Picture,
    scroll: &gtk::ScrolledWindow,
    zoom: f64,
) {
    let hadj = scroll.hadjustment();
    let vadj = scroll.vadjustment();
    let (target_x, target_y);
    {
        let mut z = state.borrow_mut();
        if !z.fit_known {
            // Nothing to anchor on yet; the initial-fit poll owns the first
            // sizing, and the user cannot have a meaningful zoom target
            // before there is a viewport.
            return;
        }
        let (old_w, old_h) = zoomed_size(z.image_w, z.image_h, z.fit, z.zoom);
        z.zoom = clamp_zoom(zoom);
        let (new_w, new_h) = zoomed_size(z.image_w, z.image_h, z.fit, z.zoom);
        target_x = anchored_value(hadj.value(), old_w as f64, new_w as f64, hadj.page_size());
        target_y = anchored_value(vadj.value(), old_h as f64, new_h as f64, vadj.page_size());
        picture.set_size_request(new_w as i32, new_h as i32);
    }
    glib::idle_add_local(move || {
        hadj.set_value(target_x);
        vadj.set_value(target_y);
        glib::ControlFlow::Break
    });
}

/// The scale that fits the picture in the viewport: the larger zoom-in that
/// still shows the whole picture, i.e. the more restrictive of the two axes.
fn fit_scale(image_w: i64, image_h: i64, view_w: f64, view_h: f64) -> f64 {
    if image_w <= 0 || image_h <= 0 || view_w <= 0.0 || view_h <= 0.0 {
        // No picture or no viewport: 1.0, so zoom math degrades to the
        // identity rather than to a division by zero.
        return 1.0;
    }
    (view_w / image_w as f64).min(view_h / image_h as f64)
}

/// The content size at a given zoom: the fit size scaled, never zero, so a
/// degenerate fit can never collapse the scrollable area to nothing.
fn zoomed_size(image_w: i64, image_h: i64, fit: f64, zoom: f64) -> (i64, i64) {
    let w = (image_w as f64 * fit * zoom).round() as i64;
    let h = (image_h as f64 * fit * zoom).round() as i64;
    (w.max(1), h.max(1))
}

fn clamp_zoom(zoom: f64) -> f64 {
    zoom.clamp(MIN_ZOOM, MAX_ZOOM)
}

/// The adjustment value that keeps the same *fraction* of the content under
/// the viewport centre after a resize from `old_size` to `new_size`.
///
/// Everything the phones' "zoom toward the tapped point" does is this
/// arithmetic with the tap point in place of the centre; the centre is what
/// a toolbar button has, so that is what this viewer uses.
fn anchored_value(old_value: f64, old_size: f64, new_size: f64, page: f64) -> f64 {
    //  the file's own test pins "already at the top: stays at the top",
    // which the original body's fraction walk does not produce; the early-out
    // is that rule.
    if old_value <= 0.0 {
        return 0.0;
    }
    let centre = old_value + page / 2.0;
    let fraction = if old_size > 0.0 {
        (centre / old_size).clamp(0.0, 1.0)
    } else {
        0.5
    };
    let wanted = fraction * new_size - page / 2.0;
    let ceiling = (new_size - page).max(0.0);
    wanted.clamp(0.0, ceiling)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fit_scale_shows_the_whole_picture_and_no_more() {
        // Width-bound: a wide photo in a short viewport.
        assert!((fit_scale(2048, 1536, 900.0, 700.0) - 900.0 / 2048.0).abs() < 1e-9);
        // Height-bound: a tall picture in a narrow viewport.
        assert!((fit_scale(700, 507, 900.0, 200.0) - 200.0 / 507.0).abs() < 1e-9);
        // Degenerate inputs degrade to the identity, not to NaN.
        assert_eq!(fit_scale(0, 0, 900.0, 700.0), 1.0);
        assert_eq!(fit_scale(100, 100, 0.0, 0.0), 1.0);
    }

    #[test]
    fn zooming_keeps_the_aspect_ratio_and_stays_in_range() {
        let fit = fit_scale(2048, 1536, 900.0, 700.0);
        let (w, h) = zoomed_size(2048, 1536, fit, clamp_zoom(50.0));
        // 900/2048 * 5, with the 4:3 ratio intact.
        assert_eq!((w, h), (4500, 3375));
        // The zoom is clamped to the phones' range, at both ends.
        assert_eq!(clamp_zoom(0.2), 1.0);
        assert_eq!(clamp_zoom(2.5), 2.5);
        assert_eq!(clamp_zoom(50.0), 5.0);
        // A degenerate fit cannot collapse the scrollable area.
        assert_eq!(zoomed_size(100, 100, 0.0, 3.0), (1, 1));
    }

    #[test]
    fn zooming_keeps_the_centre_of_view_stationary() {
        // 2000px of content under a 200px viewport showing 900..1100: the
        // centre sits at fraction 0.5, so doubling the content keeps the
        // centre at 1900 (0.5 * 4000 - 100).
        let v = anchored_value(900.0, 2000.0, 4000.0, 200.0);
        assert!((v - 1900.0).abs() < 1e-9, "{v}");
        // Already at the top: stays at the top.
        assert_eq!(anchored_value(0.0, 2000.0, 4000.0, 200.0), 0.0);
        // Near the bottom: the wanted value is past the scrollable end and
        // clamps to it (centre at 1900/2000 = 0.95 -> 3700, inside range).
        let v = anchored_value(1800.0, 2000.0, 4000.0, 200.0);
        assert!((v - 3700.0).abs() < 1e-9, "{v}");
        // The end itself clamps.
        let v = anchored_value(1900.0, 2000.0, 4000.0, 200.0);
        assert!((v - 3800.0).abs() < 1e-9, "{v}");
        // Zooming out below the viewport: the value is never negative.
        assert_eq!(anchored_value(900.0, 2000.0, 100.0, 200.0), 0.0);
        // No content to anchor on: the top, not a division by zero.
        assert_eq!(anchored_value(100.0, 0.0, 100.0, 200.0), 0.0);
    }
}
