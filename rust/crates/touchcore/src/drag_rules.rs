//! Decides whether a one-finger pan that starts at a point should move the
//! window, from what the Accessibility API reports there. Rules come from
//! probing real apps on this Mac (`--ax-probe`): toolbars, plain title bars and
//! title text move the window; text fields, tabs and value controls keep their
//! own drag behaviour; where Accessibility cannot tell, the original 28 pt rule
//! applies.

/// Window-drag zoning from Accessibility role chains.
pub struct WindowDragRules;

impl WindowDragRules {
    /// The original fixed title-bar height, used when Accessibility gives no usable answer.
    pub const FALLBACK_TITLE_BAR_HEIGHT: f64 = 28.0;

    /// How far below the window's top edge title text can still be part of the title bar.
    const TITLE_TEXT_BAND: f64 = 60.0;

    /// Roles whose own drag behaviour must win (selecting text, dragging a URL or tab, moving a value).
    const OWN_DRAG_ROLES: [&str; 12] = [
        "TextField", "TextArea", "ComboBox", "Slider", "Incrementor", "ScrollBar", "Splitter", "Cell", "Row",
        "Outline", "Table", "List",
    ];
    const OWN_DRAG_SUBROLES: [&str; 2] = ["SearchField", "TabButton"];
    /// Roles that carry no interaction of their own (backgrounds, containers, labels).
    const PASSIVE_ROLES: [&str; 8] =
        ["Window", "Group", "WebArea", "ScrollArea", "StaticText", "Image", "SplitGroup", "Unknown"];

    /// - `chain`: "Role" or "Role/Subrole" (without the "AX" prefix) from the
    ///   element under the finger up to its window; empty when Accessibility
    ///   gave no answer.
    /// - `offset`: points between the window's top edge and the touch.
    pub fn is_drag_area(chain: &[String], offset: f64) -> bool {
        let Some(first) = chain.first() else {
            return offset < Self::FALLBACK_TITLE_BAR_HEIGHT;
        };
        let (role, subrole) = split(first);
        if Self::OWN_DRAG_ROLES.contains(&role.as_str()) || subrole.is_some_and(|s| Self::OWN_DRAG_SUBROLES.contains(&s)) {
            return false;
        }
        if role == "Window" {
            return true;
        }
        if chain.iter().any(|entry| split(entry).0 == "Toolbar") {
            return true;
        }
        if (role == "StaticText" || role == "Image")
            && chain.len() > 1
            && split(&chain[1]).0 == "Window"
            && offset < Self::TITLE_TEXT_BAND
        {
            return true;
        }
        Self::PASSIVE_ROLES.contains(&role.as_str()) && offset < Self::FALLBACK_TITLE_BAR_HEIGHT
    }
}

/// Splits one chain entry into its role and, when present, its subrole.
fn split(entry: &str) -> (String, Option<&str>) {
    match entry.split_once('/') {
        Some((role, subrole)) => (role.to_owned(), Some(subrole)),
        None => (entry.to_owned(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Role chains (element first, up to the window) as reported by the
    /// Accessibility API on this Mac, captured with `TouchNext --ax-probe` on
    /// 2026-10-09. `offset` is points below the window's top edge.
    fn chain(entries: &[&str]) -> Vec<String> {
        entries.iter().map(|e| e.to_string()).collect()
    }

    #[test]
    fn safari_empty_toolbar_moves_the_window() {
        assert!(WindowDragRules::is_drag_area(&chain(&["Group", "Toolbar", "Window/StandardWindow"]), 4.0));
        assert!(WindowDragRules::is_drag_area(&chain(&["Toolbar", "Window/StandardWindow"]), 38.0));
    }

    #[test]
    fn panning_from_a_toolbar_button_moves_the_window() {
        // A tap on the button still clicks it; only a pan that starts there moves the window.
        assert!(WindowDragRules::is_drag_area(
            &chain(&["Button/Segment", "Group", "Group", "Toolbar", "Window/StandardWindow"]),
            20.0
        ));
    }

    #[test]
    fn safaris_address_field_does_not_move_the_window() {
        assert!(!WindowDragRules::is_drag_area(&chain(&["TextField", "Group", "Toolbar", "Window/StandardWindow"]), 12.0));
        assert!(!WindowDragRules::is_drag_area(
            &chain(&["TextField/SearchField", "Group/HostingView", "SplitGroup", "Window/StandardWindow"]),
            20.0
        ));
    }

    #[test]
    fn tabs_do_not_move_the_window() {
        assert!(!WindowDragRules::is_drag_area(&chain(&["RadioButton/TabButton", "Group", "Window/StandardWindow"]), 95.0));
    }

    #[test]
    fn a_plain_title_bar_and_its_title_text_move_the_window() {
        assert!(WindowDragRules::is_drag_area(&chain(&["Window/StandardWindow"]), 4.0));
        assert!(WindowDragRules::is_drag_area(&chain(&["StaticText", "Window/StandardWindow"]), 12.0));
    }

    #[test]
    fn text_directly_in_the_window_below_the_title_band_does_not_move_it() {
        assert!(!WindowDragRules::is_drag_area(&chain(&["StaticText", "Window/StandardWindow"]), 300.0));
    }

    #[test]
    fn when_accessibility_cannot_tell_the_top_28_points_still_move_the_window() {
        // Electron apps draw their own title bar as web content.
        let electron = chain(&["WebArea", "ScrollArea", "Group", "Group", "Group", "Group"]);
        assert!(WindowDragRules::is_drag_area(&electron, 10.0));
        assert!(!WindowDragRules::is_drag_area(&electron, 45.0));
        assert!(!WindowDragRules::is_drag_area(&electron, 300.0));
    }

    #[test]
    fn controls_outside_a_toolbar_do_not_move_the_window() {
        assert!(!WindowDragRules::is_drag_area(
            &chain(&["CheckBox/ToggleButton", "Group", "Group", "Group", "Group", "Group"]),
            20.0
        ));
    }

    #[test]
    fn without_accessibility_the_old_28_point_rule_applies() {
        assert!(WindowDragRules::is_drag_area(&chain(&[]), 27.0));
        assert!(!WindowDragRules::is_drag_area(&chain(&[]), 28.0));
    }
}
