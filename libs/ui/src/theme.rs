// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The tokens of ADR 0078: colours, type and space. Widgets name a role and never a
//! value, so a theme is one value a program can change or replace whole.

use aricode_gfx::{rgb, Color};
use aricode_text::{Face, INTER_REGULAR, INTER_SEMIBOLD, JETBRAINS_MONO};

/// A role for text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    LargeTitle,
    Title1,
    Title2,
    Title3,
    Headline,
    Body,
    Callout,
    Subheadline,
    /// What a person is writing: every character the same width.
    Mono,
}

/// One text style: its face, size and leading in pixels.
#[derive(Clone, Copy)]
pub struct Type {
    pub face: Face,
    pub size: u32,
    pub leading: i32,
}

const fn regular(size: u32, leading: i32) -> Type {
    Type {
        face: Face(INTER_REGULAR),
        size,
        leading,
    }
}

/// What a label's colour means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Secondary,
    Tertiary,
    Error,
}

/// What a region is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    None,
    Window,
    Content,
    Sidebar,
}

/// A distance, named by what it separates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Space {
    None,
    /// Between the controls of a group.
    Control,
    /// Between groups.
    Group,
    /// Around a window's content.
    Margin,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub window: Color,
    pub content: Color,
    pub sidebar: Color,
    pub primary: Color,
    pub secondary: Color,
    pub tertiary: Color,
    pub error: Color,
    pub accent: Color,
    pub on_accent: Color,
    /// A selection while the window has not the keys.
    pub quiet_selection: Color,
    /// Behind selected text.
    pub text_selection: Color,
    pub control: Color,
    pub control_pressed: Color,
    pub control_edge: Color,
    pub accent_pressed: Color,
    pub focus: Color,
    pub thumb: Color,

    pub control_gap: i32,
    pub group_gap: i32,
    pub margin: i32,
    pub control_h: i32,
    pub row_h: i32,
    pub list_icon: u32,
    pub radius: i32,
    /// Of a group on a card.
    pub card_radius: i32,
    pub ring: i32,
    pub button_min: i32,
    pub field_min: i32,
    /// A list's height before it is given more: rows.
    pub list_rows: i32,
    /// A dialog's width, when the window has room.
    pub dialog_w: i32,
    /// How much black lies over a window behind a dialog, and its shadow: of 256.
    pub scrim: u32,
    pub dialog_shadow: u32,
    /// Inside a button or a field, on each side.
    pub control_pad: i32,
    pub thumb_w: i32,
    pub thumb_min: i32,
    /// Rows the wheel moves per step.
    pub wheel_rows: i32,
}

/// `over` with `percent` of `color` on it.
const fn mix(color: Color, over: Color, percent: u32) -> Color {
    const fn channel(color: Color, over: Color, percent: u32, shift: u32) -> Color {
        let (c, o) = ((color >> shift) & 0xff, (over >> shift) & 0xff);
        ((c * percent + o * (100 - percent)) / 100) << shift
    }
    channel(color, over, percent, 16)
        | channel(color, over, percent, 8)
        | channel(color, over, percent, 0)
}

impl Theme {
    const fn with(window: Color, content: Color, ink: Color, sidebar: Color) -> Self {
        let accent = rgb(0x1E, 0x6E, 0xF4);
        Self {
            window,
            content,
            sidebar,
            primary: mix(ink, window, 85),
            secondary: mix(ink, window, 50),
            tertiary: mix(ink, window, 25),
            error: rgb(0xC6, 0x28, 0x28),
            accent,
            on_accent: rgb(0xff, 0xff, 0xff),
            quiet_selection: mix(ink, content, 10),
            text_selection: mix(accent, content, 30),
            control: content,
            control_pressed: mix(ink, content, 10),
            control_edge: mix(ink, window, 15),
            accent_pressed: mix(ink, accent, 15),
            focus: mix(accent, window, 50),
            thumb: mix(ink, content, 25),
            control_gap: 8,
            group_gap: 20,
            margin: 20,
            control_h: 22,
            row_h: 24,
            list_icon: 20,
            radius: 6,
            card_radius: 8,
            ring: 3,
            button_min: 64,
            field_min: 160,
            list_rows: 3,
            dialog_w: 380,
            scrim: 64,
            dialog_shadow: 77,
            control_pad: 10,
            thumb_w: 6,
            thumb_min: 20,
            wheel_rows: 3,
        }
    }

    pub const fn light() -> Self {
        Self::with(
            rgb(0xEC, 0xEC, 0xEC),
            rgb(0xFF, 0xFF, 0xFF),
            rgb(0, 0, 0),
            rgb(0xE2, 0xE2, 0xE2),
        )
    }

    pub const fn dark() -> Self {
        Self::with(
            rgb(0x32, 0x32, 0x32),
            rgb(0x1E, 0x1E, 0x1E),
            rgb(0xff, 0xff, 0xff),
            rgb(0x28, 0x28, 0x28),
        )
    }

    pub const fn of(dark: bool) -> Self {
        if dark {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub const fn style(&self, style: Style) -> Type {
        match style {
            Style::LargeTitle => regular(26, 32),
            Style::Title1 => regular(22, 26),
            Style::Title2 => regular(17, 22),
            Style::Title3 => regular(15, 20),
            Style::Headline => Type {
                face: Face(INTER_SEMIBOLD),
                size: 13,
                leading: 16,
            },
            Style::Body => regular(13, 16),
            Style::Callout => regular(12, 15),
            Style::Subheadline => regular(11, 14),
            Style::Mono => Type {
                face: Face(JETBRAINS_MONO),
                size: 13,
                leading: 18,
            },
        }
    }

    pub const fn tone(&self, tone: Tone) -> Color {
        match tone {
            Tone::Primary => self.primary,
            Tone::Secondary => self.secondary,
            Tone::Tertiary => self.tertiary,
            Tone::Error => self.error,
        }
    }

    pub const fn fill(&self, fill: Fill) -> Option<Color> {
        match fill {
            Fill::None => None,
            Fill::Window => Some(self.window),
            Fill::Content => Some(self.content),
            Fill::Sidebar => Some(self.sidebar),
        }
    }

    pub const fn space(&self, space: Space) -> i32 {
        match space {
            Space::None => 0,
            Space::Control => self.control_gap,
            Space::Group => self.group_gap,
            Space::Margin => self.margin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_adr_colours() {
        let t = Theme::light();
        assert_eq!(t.window, rgb(0xEC, 0xEC, 0xEC));
        assert_eq!(Theme::dark().content, rgb(0x1E, 0x1E, 0x1E));
        // Black at 85 % on the window.
        assert_eq!(t.primary, rgb(0x23, 0x23, 0x23));
        assert_eq!(t.accent, rgb(0x1E, 0x6E, 0xF4));
    }
}
