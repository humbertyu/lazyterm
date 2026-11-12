use std::cell::Cell;

use ratatui::style::Color;

use crate::metadata::Theme;

thread_local! {
    static ACTIVE_THEME: Cell<Theme> = const { Cell::new(Theme::Forest) };
}

#[derive(Clone, Copy)]
pub enum Role {
    Background,
    Panel,
    Border,
    Accent,
    Text,
    Muted,
    Disabled,
    Selection,
    SelectionStrong,
}

#[derive(Clone, Copy)]
pub struct ThemeColor(Role);

pub const fn color(role: Role) -> ThemeColor {
    ThemeColor(role)
}

pub fn set_active(theme: Theme) {
    ACTIVE_THEME.with(|active| active.set(theme));
}

impl From<ThemeColor> for Color {
    fn from(value: ThemeColor) -> Self {
        let theme = ACTIVE_THEME.with(Cell::get);
        let (red, green, blue) = match (theme, value.0) {
            (Theme::Forest, Role::Background) => (13, 16, 17),
            (Theme::Forest, Role::Panel) => (18, 23, 23),
            (Theme::Forest, Role::Border) => (49, 65, 61),
            (Theme::Forest, Role::Accent) => (133, 196, 159),
            (Theme::Forest, Role::Text) => (215, 228, 218),
            (Theme::Forest, Role::Muted) => (107, 126, 116),
            (Theme::Forest, Role::Disabled) => (76, 91, 82),
            (Theme::Forest, Role::Selection) => (47, 78, 61),
            (Theme::Forest, Role::SelectionStrong) => (34, 54, 47),
            (Theme::Ocean, Role::Background) => (12, 18, 25),
            (Theme::Ocean, Role::Panel) => (17, 27, 36),
            (Theme::Ocean, Role::Border) => (48, 73, 88),
            (Theme::Ocean, Role::Accent) => (102, 190, 218),
            (Theme::Ocean, Role::Text) => (219, 231, 237),
            (Theme::Ocean, Role::Muted) => (121, 148, 160),
            (Theme::Ocean, Role::Disabled) => (75, 95, 105),
            (Theme::Ocean, Role::Selection) => (32, 67, 84),
            (Theme::Ocean, Role::SelectionStrong) => (23, 46, 58),
            (Theme::Amber, Role::Background) => (21, 17, 12),
            (Theme::Amber, Role::Panel) => (30, 24, 17),
            (Theme::Amber, Role::Border) => (80, 65, 42),
            (Theme::Amber, Role::Accent) => (232, 177, 91),
            (Theme::Amber, Role::Text) => (240, 229, 210),
            (Theme::Amber, Role::Muted) => (161, 143, 113),
            (Theme::Amber, Role::Disabled) => (99, 83, 59),
            (Theme::Amber, Role::Selection) => (88, 61, 26),
            (Theme::Amber, Role::SelectionStrong) => (54, 39, 20),
            (Theme::Rose, Role::Background) => (22, 15, 19),
            (Theme::Rose, Role::Panel) => (32, 21, 27),
            (Theme::Rose, Role::Border) => (84, 48, 61),
            (Theme::Rose, Role::Accent) => (225, 146, 169),
            (Theme::Rose, Role::Text) => (239, 224, 231),
            (Theme::Rose, Role::Muted) => (162, 126, 140),
            (Theme::Rose, Role::Disabled) => (100, 75, 85),
            (Theme::Rose, Role::Selection) => (83, 40, 59),
            (Theme::Rose, Role::SelectionStrong) => (52, 27, 39),
        };
        Color::Rgb(red, green, blue)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palettes_are_distinct_and_all_use_dark_backgrounds() {
        let themes = [Theme::Forest, Theme::Ocean, Theme::Amber, Theme::Rose];
        let backgrounds = themes.map(|theme| {
            set_active(theme);
            Color::from(color(Role::Background))
        });
        assert_eq!(backgrounds[0], Color::Rgb(13, 16, 17));
        assert!(backgrounds.iter().all(|color| {
            matches!(color, Color::Rgb(red, green, blue) if *red < 40 && *green < 40 && *blue < 40)
        }));
        assert!(backgrounds.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn palettes_serialize_round_trip() {
        for theme in Theme::ALL {
            let encoded = serde_json::to_string(&theme).unwrap();
            assert_eq!(serde_json::from_str::<Theme>(&encoded).unwrap(), theme);
        }
    }
}
