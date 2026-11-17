//! A picture whose height follows the width it is given.
//!
//! **Why this exists.** The note preview is a vertical `GtkBox` inside a
//! `ScrolledWindow`, and GTK's box layout gives every child its *minimum*
//! size. `GtkPicture` with `can_shrink(true)` reports a minimum height of
//! **0** — its natural height is the aspect-correct one, but a box hands out
//! minimums, not naturals, so the picture was allocated `900x0` and the image
//! never painted. The notes with images showed a bare gap where the picture
//! should be.
//!
//! `can_shrink(false)` fixes the height but breaks the width: the picture's
//! minimum width becomes the image's own width (2048 for a photo), the
//! column is then laid out at 2048 and the whole note reads as a wall of
//! text that the pane clips.
//!
//! **What this widget does.** It reports a minimum width of 0 (a wide image
//! never widens the column) and a minimum *height* computed from the width
//! the box gives it (`width * height / width_of_image`). It requests
//! height-for-width, so GTK re-measures it whenever the pane is resized and
//! the height follows automatically.
//!
//! The image is drawn scaled into the allocation, centred, so even a
//! mismatched allocation shows the whole picture rather than a crop.

use std::cell::RefCell;

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use gtk4::{Orientation, SizeRequestMode};

use gtk4 as gtk;

mod imp {
    use super::*;

    /// The widget's state: the decoded texture, if there is one.
    #[derive(Default)]
    pub struct AspectImage {
        pub texture: RefCell<Option<gtk::gdk::Texture>>,
        /// The size to report before a texture arrives — the space a loading
        /// image reserves so the page under it does not jump. Measuring with
        /// it is what `imgcache`'s placeholders are for; a widget built with
        /// a texture never reads it.
        pub placeholder: std::cell::Cell<(i32, i32)>,
        /// A hard cap: the widget fits *inside* this box instead of
        /// reporting its picture's own size. A shelf's cover thumbnail
        /// needs this — without it, the picture's natural height is also
        /// its minimum height, and the card grew to the size of the cover
        /// file however small the card asked to be.
        pub cap: std::cell::Cell<(i32, i32)>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AspectImage {
        const NAME: &'static str = "SnAspectImage";
        type Type = super::AspectImage;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for AspectImage {}

    impl WidgetImpl for AspectImage {
        /// Height depends on the width: the box asks "how tall, given this
        /// width?" and the answer follows the picture's own aspect ratio.
        fn request_mode(&self) -> SizeRequestMode {
            SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (w, h) = self.image_size();
            //  the original `cap` field's contract — `set_cap`'s own doc
            // says "the measure reads it", but the reading half was lost.
            // Without it a capped widget (the shelf's cover thumbnail) still
            // reports the decoded image's own natural size, the homogeneous
            // FlowBox cell takes that from the largest card, and the shelf
            // renders one giant column whatever size mode or column count
            // the user picks.
            let cap = self.cap.get();
            let capped = |v: i32, max: i32| if max > 0 { v.min(max) } else { v };
            match orientation {
                Orientation::Horizontal => {
                    // A minimum of 0: a 2048px-wide photo must not widen the
                    // note's column (the preview has no horizontal scrollbar,
                    // so a wider column is a clipped note). The natural width
                    // is the image's own width, but capped by the width the
                    // pane offered — a column that asks for more than the
                    // pane has is a clipped note again.
                    let offered = if for_size > 0 { w.min(for_size) } else { w };
                    let natural = capped(offered, cap.0);
                    (0, natural, -1, -1)
                }
                Orientation::Vertical => {
                    let width = if for_size > 0 { for_size } else { w };
                    let height = capped(aspect_height(w, h, width), cap.1);
                    // Minimum *and* natural are the same: the box allocates
                    // the minimum, and this is the height the picture needs.
                    (height, height, -1, -1)
                }
                _ => (0, 0, -1, -1),
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let texture = self.texture.borrow().clone();
            let Some(texture) = texture else {
                return;
            };
            let (w, h) = self.image_size();
            if w <= 0 || h <= 0 {
                return;
            }
            let alloc_w = self.obj().width() as f32;
            let alloc_h = self.obj().height() as f32;
            if alloc_w <= 0.0 || alloc_h <= 0.0 {
                return;
            }
            // Fit the picture inside the allocation, keeping its aspect.
            let scale = (alloc_w / w as f32).min(alloc_h / h as f32);
            let draw_w = w as f32 * scale;
            let draw_h = h as f32 * scale;
            let rect = gtk::graphene::Rect::new(
                (alloc_w - draw_w) / 2.0,
                (alloc_h - draw_h) / 2.0,
                draw_w,
                draw_h,
            );
            // Rounded corners, the way iOS clips a note image to its 8pt
            // rounded rect. GTK's CSS `border-radius` rounds a widget's
            // background only; a texture this widget draws itself sits on top
            // of that unclipped, so the clip is ours to push. The radius is
            // the theme's answer (8 px, or 0 under E-Ink's square edges) so a
            // scheme switch is the only thing that changes it.
            snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(
                gtk::graphene::Rect::new(0.0, 0.0, alloc_w, alloc_h),
                crate::theme::image_corner_radius(),
            ));
            snapshot.append_scaled_texture(
                &texture,
                gtk::gsk::ScalingFilter::Linear,
                &rect,
            );
            snapshot.pop();
        }
    }

    impl AspectImage {
        /// The picture's own size, or `(0, 0)` when there is none.
        fn image_size(&self) -> (i32, i32) {
            let texture = self.texture.borrow();
            match texture.as_ref() {
                Some(t) => (t.intrinsic_width(), t.intrinsic_height()),
                None => self.placeholder.get(),
            }
        }
    }
}

glib::wrapper! {
    /// A picture that takes its height from the width it is given.
    pub struct AspectImage(ObjectSubclass<imp::AspectImage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AspectImage {
    /// A picture showing `texture`, sized by its aspect ratio.
    pub fn new(texture: &gtk::gdk::Texture) -> Self {
        let widget: Self = glib::Object::new();
        widget.imp().texture.replace(Some(texture.clone()));
        widget
    }

    /// The space a picture of `w`×`h` will occupy, before it exists — a
    /// loading placeholder that reserves the page's shape.
    pub fn new_placeholder(w: u32, h: u32) -> Self {
        let widget: Self = glib::Object::new();
        widget.imp().placeholder.set((w as i32, h as i32));
        widget
    }

    /// The picture this widget draws, when it has one.
    pub fn texture(&self) -> Option<gtk::gdk::Texture> {
        self.imp().texture.borrow().clone()
    }

    /// Swap the picture in — the async decode path's landing. The re-layout
    /// matters: the placeholder's size and the picture's are the same by
    /// construction, but the allocation is only recomputed when asked.
    pub fn set_texture(&self, texture: gtk::gdk::Texture) {
        self.imp().texture.replace(Some(texture));
        self.queue_resize();
    }

    /// Cap the widget's drawn size: it fits *inside* this box instead of
    /// filling its row. The imgcache's async path hands the cap in before the
    /// picture lands, and the measure reads it (see `imp::AspectImage::cap`).
    //  authored from the call sites (imgcache.rs
    // `aspect.set_cap(width, height)`); the cap field itself is original, the
    // setter line did not survive.
    pub fn set_cap(&self, width: i32, height: i32) {
        self.imp().cap.set((width, height));
        self.queue_resize();
    }
}

/// The height a picture of `image_w` x `image_h` pixels needs at `width`.
///
/// The picture fills the width it is given — the note preview's images are
/// full-width, the way the iOS reader shows them — so the height is the
/// width scaled by the image's own aspect ratio.
fn aspect_height(image_w: i32, image_h: i32, width: i32) -> i32 {
    if image_w <= 0 || image_h <= 0 || width <= 0 {
        return 0;
    }
    ((width as i64 * image_h as i64 + image_w as i64 / 2) / image_w as i64) as i32
}

#[cfg(test)]
mod tests {
    use super::aspect_height;

    #[test]
    fn the_height_follows_the_picture_s_aspect_ratio() {
        // A 700x507 picture at 900px wide is 652px tall (rounded).
        assert_eq!(aspect_height(700, 507, 900), 652);
        // A square picture stays square.
        assert_eq!(aspect_height(500, 500, 900), 900);
        // A wide picture is short.
        assert_eq!(aspect_height(2048, 1536, 900), 675);
        // No picture, no height.
        assert_eq!(aspect_height(0, 0, 900), 0);
    }
}
