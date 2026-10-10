use serde::Deserialize;
use serde::Serialize;

pub const MAX_USER_INPUT_TEXT_CHARS: usize = 1 << 20;

pub fn local_image_label_text(label_number: usize) -> String {
    format!("[Image #{label_number}]")
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct TextElement {
    pub byte_range: ByteRange,
    placeholder: Option<String>,
}

impl TextElement {
    pub fn new(byte_range: ByteRange, placeholder: Option<String>) -> Self {
        Self {
            byte_range,
            placeholder,
        }
    }

    pub fn map_range<F>(&self, map: F) -> Self
    where
        F: FnOnce(ByteRange) -> ByteRange,
    {
        Self {
            byte_range: map(self.byte_range),
            placeholder: self.placeholder.clone(),
        }
    }

    pub fn set_placeholder(&mut self, placeholder: Option<String>) {
        self.placeholder = placeholder;
    }

    #[doc(hidden)]
    pub fn _placeholder_for_conversion_only(&self) -> Option<&str> {
        self.placeholder.as_deref()
    }

    pub fn placeholder<'a>(&'a self, text: &'a str) -> Option<&'a str> {
        self.placeholder
            .as_deref()
            .or_else(|| text.get(self.byte_range.start..self.byte_range.end))
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub struct ByteRange {
    pub start: usize,
    pub end: usize,
}

impl From<std::ops::Range<usize>> for ByteRange {
    fn from(range: std::ops::Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_element_wire_shape_is_byte_compatible() {
        let element = TextElement::new(ByteRange { start: 3, end: 9 }, None);
        assert_eq!(
            serde_json::to_value(&element).expect("serialize"),
            serde_json::json!({"byte_range": {"start": 3, "end": 9}, "placeholder": null})
        );

        let named = TextElement::new((0..4).into(), Some("[img]".to_string()));
        assert_eq!(
            serde_json::to_value(&named).expect("serialize"),
            serde_json::json!({"byte_range": {"start": 0, "end": 4}, "placeholder": "[img]"})
        );

        let decoded: TextElement = serde_json::from_value(
            serde_json::json!({"byte_range": {"start": 0, "end": 4}, "placeholder": "[img]"}),
        )
        .expect("decode");
        assert_eq!(decoded, named);
    }

    #[test]
    fn placeholder_falls_back_to_the_text_slice() {
        let element = TextElement::new((2..5).into(), None);
        assert_eq!(element.placeholder("abcdef"), Some("cde"));
        let named = TextElement::new((2..5).into(), Some("chip".to_string()));
        assert_eq!(named.placeholder("abcdef"), Some("chip"));
    }
}
