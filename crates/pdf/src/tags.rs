use super::krilla::tagging::ListNumbering;
use super::{ArtifactType, Draw, GraphicKind, Node, Tag, TagGroup, TagKind};
use wordcraft_doc::section::NumFormat;

/// A chart's or diagram's alt text: its own, else what it is ("chart", "diagram").
pub(crate) fn graphic_alt(alt: &str, kind: GraphicKind) -> &str {
    if alt.trim().is_empty() { kind.noun() } else { alt }
}

/// A picture, or a rotation or clip holding one at any depth (a rotated group's turned members, a
/// turned picture cut at its line).
pub(crate) fn holds_picture(d: &Draw) -> bool {
    match d {
        Draw::Image { .. } => true,
        Draw::Rotated { items, .. } | Draw::Clip { items, .. } => items.iter().any(holds_picture),
        _ => false,
    }
}

/// Text, or a rotation (or clip) holding it at any depth: a turned text box's lines.
pub(crate) fn holds_text(d: &Draw) -> bool {
    match d {
        Draw::Glyphs { .. } | Draw::Turned { .. } => true,
        Draw::Rotated { items, .. } | Draw::Clip { items, .. } => items.iter().any(holds_text),
        _ => false,
    }
}

/// The alt text of an inline figure: a graphic's (see [`graphic_alt`]) or a described shape's.
pub(crate) fn figure_alt(alt: &str, kind: Option<GraphicKind>) -> &str {
    match kind {
        Some(k) => graphic_alt(alt, k),
        None => alt,
    }
}

/// A structure element being collected (one per paragraph, in reading order).
pub(crate) struct TagEntry {
    pub(crate) kind: TagKind,
    pub(crate) children: Vec<Node>,
    /// A list paragraph's list: its `num` id, level and how its labels look. Consecutive entries
    /// of one list become one `L` with an `LI` each (see [`structure`]).
    pub(crate) list: Option<ListItem>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct ListItem {
    pub(crate) num: u32,
    pub(crate) level: u8,
    pub(crate) numbering: ListNumbering,
}

/// The list attribute for a level's label format (PDF 1.7 §14.8.5.5, `ListNumbering`).
pub(crate) fn list_numbering(format: NumFormat, text: &str) -> ListNumbering {
    match format {
        NumFormat::Bullet => match text.trim() {
            "\u{25CB}" | "o" => ListNumbering::Circle,
            "\u{25A0}" | "\u{25AA}" | "\u{F0A7}" => ListNumbering::Square,
            _ => ListNumbering::Disc,
        },
        NumFormat::None => ListNumbering::None,
        NumFormat::UpperRoman => ListNumbering::UpperRoman,
        NumFormat::LowerRoman => ListNumbering::LowerRoman,
        NumFormat::UpperLetter => ListNumbering::UpperAlpha,
        NumFormat::LowerLetter => ListNumbering::LowerAlpha,
        _ => ListNumbering::Decimal,
    }
}

/// One open list while [`structure`] nests list paragraphs: its level, label kind and the
/// children of each of its items so far.
struct OpenList {
    level: u8,
    numbering: ListNumbering,
    items: Vec<Vec<Node>>,
}

fn close(list: OpenList) -> Node {
    let items = list.items.into_iter().map(|children| Node::Group(TagGroup::with_children(Tag::LI, children))).collect();
    Node::Group(TagGroup::with_children(Tag::L(list.numbering), items))
}

/// Closes every open list, each into the last item of the one around it, the outermost into `out`.
fn finish(open: &mut Vec<OpenList>, out: &mut Vec<Node>) {
    while let Some(list) = open.pop() {
        let node = close(list);
        match open.last_mut().and_then(|parent| parent.items.last_mut()) {
            Some(item) => item.push(node),
            None => out.push(node),
        }
    }
}

/// The structure tree's top-level elements for `entries` (in reading order): paragraphs as they
/// are, empty ones left out, and each run of paragraphs of one list as an `L` whose items are
/// `LI` > `LBody` (the label is drawn with the text, so there is no separate `Lbl`). A deeper
/// level is a list inside the previous item; a level back up closes it.
pub(crate) fn structure(entries: Vec<TagEntry>) -> Vec<Node> {
    let mut out = Vec::new();
    let mut open: Vec<OpenList> = Vec::new();
    let mut current: Option<u32> = None;
    for e in entries {
        if e.children.is_empty() {
            continue;
        }
        let Some(item) = e.list else {
            finish(&mut open, &mut out);
            current = None;
            out.push(Node::Group(TagGroup::with_children(e.kind, e.children)));
            continue;
        };
        if current != Some(item.num) {
            finish(&mut open, &mut out);
            current = Some(item.num);
        }
        while open.len() > 1 && open.last().is_some_and(|l| l.level > item.level) {
            let inner = open.pop().map(close);
            if let (Some(node), Some(parent)) = (inner, open.last_mut().and_then(|p| p.items.last_mut())) {
                parent.push(node);
            }
        }
        if open.last().is_none_or(|l| item.level > l.level && !l.items.is_empty()) {
            open.push(OpenList { level: item.level, numbering: item.numbering, items: Vec::new() });
        }
        let body = Node::Group(TagGroup::with_children(Tag::LBody, e.children));
        if let Some(list) = open.last_mut() {
            list.items.push(vec![body]);
        }
    }
    finish(&mut open, &mut out);
    out
}

/// What the content being drawn is, for tagging.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Role {
    /// Paragraph content of the structure entry at this index.
    Para(usize),
    /// Header/footer, decorations: not part of the structure.
    Artifact(ArtifactType),
    /// A floating picture (its own Figure).
    Figure,
}

#[cfg(test)]
mod holds_tests {
    use super::{Draw, holds_text};

    /// A rotated shape with no text isn't the paragraph's text; a turned text box's lines are.
    #[test]
    fn only_a_rotation_holding_text_holds_text() {
        let turned = |items| Draw::Rotated { cx: 0.0, cy: 0.0, spin: Default::default(), items };
        let text = Draw::Turned { x: 0.0, y: 0.0, turn: Default::default(), items: Vec::new() };
        assert!(!holds_text(&turned(Vec::new())));
        assert!(!holds_text(&turned(vec![turned(Vec::new())])));
        assert!(holds_text(&turned(vec![text.clone()])));
        assert!(holds_text(&turned(vec![Draw::Clip { rect: Default::default(), items: vec![turned(vec![text])] }])));
    }
}
