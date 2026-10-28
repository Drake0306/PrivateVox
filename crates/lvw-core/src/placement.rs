//! Where the pill goes: bottom-centre of the work area above the Dock, or where the user last dragged it.

/// A monitor's usable area in physical pixels (menu bar and Dock excluded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Bottom-centre of `area`, `margin` pixels above its bottom edge.
pub fn default_pill_position(area: &Rect, pill: (u32, u32), margin: u32) -> (i32, i32) {
    let x = area.x + (area.w as i32 - pill.0 as i32) / 2;
    let y = area.y + area.h as i32 - pill.1 as i32 - margin as i32;
    (x, y)
}

/// A saved position is used only if the whole pill still fits inside `area`.
pub fn clamp_to_area(saved: (i32, i32), area: &Rect, pill: (u32, u32)) -> Option<(i32, i32)> {
    let (x, y) = saved;
    let fits_x = x >= area.x && x + pill.0 as i32 <= area.x + area.w as i32;
    let fits_y = y >= area.y && y + pill.1 as i32 <= area.y + area.h as i32;
    if fits_x && fits_y {
        Some(saved)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect { x: 0, y: 25, w: 1440, h: 800 }; // menu bar 25 px, Dock already excluded
    const PILL: (u32, u32) = (280, 48);

    #[test]
    fn default_is_bottom_centre_with_margin() {
        let p = default_pill_position(&AREA, PILL, 12);
        assert_eq!(p, (580, 25 + 800 - 48 - 12));
    }

    #[test]
    fn default_on_a_second_monitor_uses_its_origin() {
        let area = Rect { x: 1440, y: 0, w: 1000, h: 600 };
        assert_eq!(default_pill_position(&area, PILL, 12), (1440 + 360, 600 - 48 - 12));
    }

    #[test]
    fn saved_position_inside_area_is_kept() {
        assert_eq!(clamp_to_area((100, 700), &AREA, PILL), Some((100, 700)));
    }

    #[test]
    fn saved_position_partly_off_screen_is_rejected() {
        assert_eq!(clamp_to_area((1300, 700), &AREA, PILL), None, "pill would run past the right edge");
        assert_eq!(clamp_to_area((100, 10), &AREA, PILL), None, "pill would sit under the menu bar");
        assert_eq!(clamp_to_area((-50, 700), &AREA, PILL), None);
    }
}
