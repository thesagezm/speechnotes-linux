//! Accessible names and values — the parts of the UI that are driven from
//! outside.
//!
//! Why this exists
//! ===============
//! Every feature in this app has to be reviewable without a human hand on
//! the mouse: the gate drives the app through AT-SPI, clicking controls by
//! name and reading their state back. Two GTK defaults make that impossible:
//!
//! * **An icon-only button has no name.** A `gtk::Button` built with an
//!   `icon_name` and nothing else announces as "button" — the screen
//!   reader and the gate both get nothing to aim at. The tooltip is the
//!   human wording, so it is also the accessible label.
//! * **A `gtk::DropDown` cannot be selected through AT-SPI.** Its popup
//!   rows expose only `scroll-to`; `Selection::select_child` returns
//!   success and changes nothing. A dropdown is therefore *readable* from
//!   outside but not *drivable*, which is exactly the half the gate needs.
//!
//! The fix for the second is to give the dropdown a plain button beside it
//! — a real widget with a real action — rather than to fight AT-SPI. The
//! helper below builds that button and keeps it in step with the dropdown,
//! so the choice can be changed from outside and the dropdown still owns
//! the single source of truth.
//!
//! Everything here is *additive*: it never changes what a control does,
//! only how it is named and how it can be reached.

use gtk4::prelude::*;
use gtk4 as gtk;

/// Name a control so it can be found by name.
///
/// A label already has one; this only fills the gap for controls built
/// from an icon or a tooltip alone.
//  the original bound (`AccessibleExtManual` + `downcast_ref`) does not
// exist on gtk4 0.9's generic Accessible; `IsA<Widget>` pins the same
// call sites (buttons, labels, dropdowns) through `upcast_ref`.
pub fn label<T: gtk::prelude::IsA<gtk::Widget>>(control: &T, name: &str) {
    control
        .upcast_ref::<gtk::Widget>()
        .update_property(&[gtk::accessible::Property::Label(name)]);
}

/// Name a control *and* give it a tooltip, from one string.
///
/// The tooltip is what a sighted mouse user reads, so the two should never
/// drift apart.
pub fn name_with_tip<T: gtk::prelude::IsA<gtk::Widget>>(control: &T, name: &str) {
    let widget = control.upcast_ref::<gtk::Widget>();
    widget.set_tooltip_text(Some(name));
    widget.update_property(&[gtk::accessible::Property::Label(name)]);
}

/// A button that steps a dropdown one item forward, and one back.
///
/// The dropdown's own popup cannot be operated through AT-SPI (see the
/// module header), so this is what makes a dropdown *drivable*: the pair
/// reads as "previous value / the value / next value", and clicking the
/// arrows changes the selection exactly as picking a row would.
///
/// The pair is returned as `(previous, next)`; the caller puts them
/// wherever the control reads best.
pub fn dropdown_steppers(
    dropdown: &gtk::DropDown,
) -> (gtk::Button, gtk::Button) {
    let previous = gtk::Button::builder()
        .icon_name("go-up-symbolic")
        .tooltip_text("Previous value")
        .build();
    let next = gtk::Button::builder()
        .icon_name("go-down-symbolic")
        .tooltip_text("Next value")
        .build();
    label(&previous, "Previous value");
    label(&next, "Next value");

    {
        let dropdown = dropdown.clone();
        previous.connect_clicked(move |_| {
            //  gtk4 0.9's `model()` returns the ListModel, not a count; the
            // original's expression read against an older API shape.
            let n = dropdown.model().map(|m| m.n_items()).unwrap_or(0);
            if n == 0 {
                return;
            }
            let current = dropdown.selected();
            dropdown.set_selected(if current == 0 { n - 1 } else { current - 1 });
        });
    }
    {
        let dropdown = dropdown.clone();
        next.connect_clicked(move |_| {
            //  gtk4 0.9's `model()` returns the ListModel, not a count; the
            // original's expression read against an older API shape.
            let n = dropdown.model().map(|m| m.n_items()).unwrap_or(0);
            if n == 0 {
                return;
            }
            let current = dropdown.selected();
            dropdown.set_selected(if current + 1 >= n { 0 } else { current + 1 });
        });
    }
    (previous, next)
}

/// The stepper pair, placed on `row` right after `dropdown`.
///
///  authored from the call sites (pomodoro.rs
/// `steppers_after(&mode_drop, &mode_row)`; window.rs uses
/// `dropdown_steppers(&shelf_columns)` directly); the function's own name and
/// its doc-commented contract ("keeps it in step with the dropdown, so the
/// choice can be changed from outside") come from the original a11y module
/// header. The final a11y.rs body carrying it did not survive.
pub fn steppers_after(dropdown: &gtk::DropDown, row: &gtk::Box) {
    let (previous, next) = dropdown_steppers(dropdown);
    row.append(&previous);
    row.append(&next);
}

/// The dropdown's current value, as text — for a gate that reads state.
pub fn dropdown_value(dropdown: &gtk::DropDown) -> String {
    dropdown
        .selected_item()
        //  gtk4 0.9's selected_item() yields a plain Object; the original
        // called ListItem::child on it directly.
        .and_then(|item| item.downcast::<gtk::ListItem>().ok())
        .and_then(|item| item.child())
        .and_then(|child| child.downcast::<gtk::Label>().ok())
        .map(|label| label.text().to_string())
        .unwrap_or_default()
}

/// Keep a dropdown's accessible label in step with its value.
///
/// A dropdown announces its *current value*, which is right for a screen
/// reader but means a gate cannot find "the columns dropdown" by a stable
/// name — the name changes when the value changes. Pinning the label to
/// the control's purpose gives it a stable handle, and the value is still
/// readable from the accessible name GTK gives the list item.
pub fn stable_dropdown_label(dropdown: &gtk::DropDown, purpose: &str) {
    dropdown.update_property(&[gtk::accessible::Property::Label(purpose)]);
    dropdown.set_tooltip_text(Some(purpose));
}
