use crossterm::event::KeyCode;

#[derive(Clone, Debug, Default)]
pub struct Editor {
    field: usize,
    cursor: Option<usize>,
    pub error: Option<String>,
}

impl Editor {
    pub fn position(&self, field: usize, value: &str) -> usize {
        let mut cursor = if self.field == field {
            self.cursor.unwrap_or(value.len()).min(value.len())
        } else {
            value.len()
        };
        while !value.is_char_boundary(cursor) {
            cursor -= 1;
        }
        cursor
    }

    pub fn reset(&mut self) {
        self.cursor = None;
    }

    pub fn edit(&mut self, field: usize, value: &mut String, key: KeyCode) -> bool {
        let mut cursor = self.position(field, value);
        let previous = || {
            value[..cursor]
                .char_indices()
                .next_back()
                .map_or(0, |(index, _)| index)
        };
        let next = || {
            value[cursor..]
                .chars()
                .next()
                .map_or(cursor, |character| cursor + character.len_utf8())
        };
        match key {
            KeyCode::Left => cursor = previous(),
            KeyCode::Right => cursor = next(),
            KeyCode::Home => cursor = 0,
            KeyCode::End => cursor = value.len(),
            KeyCode::Backspace if cursor > 0 => {
                let start = previous();
                value.drain(start..cursor);
                cursor = start;
                self.error = None;
            }
            KeyCode::Delete if cursor < value.len() => {
                let end = next();
                value.drain(cursor..end);
                self.error = None;
            }
            KeyCode::Char(character) if !character.is_control() => {
                value.insert(cursor, character);
                cursor += character.len_utf8();
                self.error = None;
            }
            KeyCode::Backspace | KeyCode::Delete => {}
            _ => return false,
        }
        self.field = field;
        self.cursor = Some(cursor);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_unicode_in_the_middle_and_keeps_field_positions_valid() {
        let mut editor = Editor::default();
        let mut value = "a中文z".to_string();
        editor.edit(0, &mut value, KeyCode::Left);
        editor.edit(0, &mut value, KeyCode::Left);
        editor.edit(0, &mut value, KeyCode::Char('日'));
        assert_eq!(value, "a中日文z");
        editor.edit(0, &mut value, KeyCode::Delete);
        assert_eq!(value, "a中日z");
        editor.edit(0, &mut value, KeyCode::Backspace);
        assert_eq!(value, "a中z");
        editor.edit(0, &mut value, KeyCode::Home);
        editor.edit(0, &mut value, KeyCode::Delete);
        assert_eq!(value, "中z");
        editor.edit(0, &mut value, KeyCode::End);
        assert_eq!(editor.position(0, &value), value.len());
        assert_eq!(editor.position(1, "other"), 5);
        assert!(editor.position(0, "日") <= "日".len());
        editor.reset();
        assert_eq!(editor.position(0, "replacement"), 11);
        assert!(!editor.edit(0, &mut value, KeyCode::Enter));
    }
}
