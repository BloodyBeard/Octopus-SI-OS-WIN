#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTextChunk<T> {
    pub visible_text: String,
    pub extracted: Vec<T>,
}

impl<T> Default for StreamTextChunk<T> {
    fn default() -> Self {
        Self {
            visible_text: String::new(),
            extracted: Vec::new(),
        }
    }
}

impl<T> StreamTextChunk<T> {
    pub fn is_empty(&self) -> bool {
        self.visible_text.is_empty() && self.extracted.is_empty()
    }
}

pub trait StreamTextParser {
    type Extracted;

    fn push_str(&mut self, chunk: &str) -> StreamTextChunk<Self::Extracted>;

    fn finish(&mut self) -> StreamTextChunk<Self::Extracted>;
}
