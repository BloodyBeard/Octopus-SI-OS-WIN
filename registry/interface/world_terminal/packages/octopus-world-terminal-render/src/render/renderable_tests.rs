use super::*;
use pretty_assertions::assert_eq;

struct HeightRenderable(u16);

impl HeightRenderable {
    fn with_height(height: u16) -> Self {
        Self(height)
    }
}

impl Renderable for HeightRenderable {
    fn render(&self, _area: Rect, _buf: &mut Buffer) {}

    fn desired_height(&self, _width: u16) -> u16 {
        self.0
    }
}

#[test]
fn flex_redistributes_space_unused_by_short_children() {
    let mut flex = FlexRenderable::new();
    flex.push(
        1,
        RenderableItem::Owned(Box::new(HeightRenderable::with_height(20))),
    );
    flex.push(
        1,
        RenderableItem::Owned(Box::new(HeightRenderable::with_height(2))),
    );

    let allocated = flex.allocate(Rect::new(0, 0, 80, 10));

    assert_eq!(
        allocated
            .into_iter()
            .map(|area| area.height)
            .collect::<Vec<_>>(),
        vec![8, 2],
    );
}

#[test]
fn flex_reserves_non_flex_space_before_flexible_children() {
    let mut flex = FlexRenderable::new();
    flex.push(
        1,
        RenderableItem::Owned(Box::new(HeightRenderable::with_height(20))),
    );
    flex.push(
        0,
        RenderableItem::Owned(Box::new(HeightRenderable::with_height(2))),
    );
    flex.push(
        1,
        RenderableItem::Owned(Box::new(HeightRenderable::with_height(20))),
    );

    let allocated = flex.allocate(Rect::new(0, 0, 80, 10));

    assert_eq!(
        allocated
            .into_iter()
            .map(|area| area.height)
            .collect::<Vec<_>>(),
        vec![4, 2, 4],
    );
}
