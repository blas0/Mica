//! Where the panes are.
//!
//! A window's area is a tree: every node is either a pane or a split of two
//! subtrees. Splitting a pane replaces that leaf with a split node holding the
//! old pane and the new one; closing a pane replaces its parent split with
//! whichever sibling survived. Nothing else moves, which is what makes a split
//! feel local rather than like a relayout.
//!
//! ## Cells, not pixels
//!
//! Every rectangle here is measured in grid cells, and every divider is one
//! cell wide. That is not a simplification — it is what lets the renderer draw
//! every pane in a single pass. Instances carry cell coordinates, so a pane is
//! just an offset rectangle in one shared grid, and N panes cost one draw call
//! rather than N.
//!
//! The consequence is that a split lands on a cell boundary, and a ratio is
//! honoured only as closely as the cell size allows. A terminal has always
//! worked that way.

/// A pane's identity. Stable across splits and closes of *other* panes, so the
/// focused pane stays focused when its neighbour goes away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PaneId(pub u32);

/// A rectangle of the grid, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellRect {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
}

impl CellRect {
    pub fn new(col: u16, row: u16, cols: u16, rows: u16) -> CellRect {
        CellRect { col, row, cols, rows }
    }

    pub fn contains(&self, col: u16, row: u16) -> bool {
        col >= self.col
            && row >= self.row
            && col < self.col.saturating_add(self.cols)
            && row < self.row.saturating_add(self.rows)
    }

    fn centre(&self) -> (f32, f32) {
        (
            self.col as f32 + self.cols as f32 / 2.0,
            self.row as f32 + self.rows as f32 / 2.0,
        )
    }
}

/// Which way the new pane goes. The same four words the bindings use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Columns,
            Direction::Up | Direction::Down => Axis::Rows,
        }
    }

    /// True when the new pane takes the first half — left or top.
    fn leads(self) -> bool {
        matches!(self, Direction::Left | Direction::Up)
    }
}

/// What a split divides.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// Side by side, divided by a vertical rule.
    Columns,
    /// Stacked, divided by a horizontal rule.
    Rows,
}

/// The smallest pane worth having. Below this a shell cannot draw a prompt and
/// the split is refused rather than made and immediately unusable.
pub const MIN_COLS: u16 = 12;
pub const MIN_ROWS: u16 = 3;

/// One cell of the grid, given over to the line between two panes.
pub const DIVIDER: u16 = 1;

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Leaf(PaneId),
    Split { axis: Axis, ratio: f32, first: Box<Node>, second: Box<Node> },
}

/// The tree, and nothing else — no sessions, no renderer, no AppKit. Every
/// question the window layer asks about layout is answered here, which is why
/// all of it can be tested without opening a window.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    root: Node,
}

/// A divider, for drawing and for dragging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    pub axis: Axis,
    /// The cell the rule occupies: a column for [`Axis::Columns`], a row for
    /// [`Axis::Rows`].
    pub at: u16,
    /// Where the rule starts and how far it runs, across the other axis.
    pub start: u16,
    pub len: u16,
}

impl Layout {
    pub fn new(root: PaneId) -> Layout {
        Layout { root: Node::Leaf(root) }
    }

    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    pub fn len(&self) -> usize {
        self.panes().len()
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Splits `target`, putting `new` on the given side of it.
    ///
    /// Refuses — and changes nothing — when the halves would come out below
    /// [`MIN_COLS`] / [`MIN_ROWS`]. A refusal the caller can report is better
    /// than a two-column pane.
    pub fn split(
        &mut self,
        area: CellRect,
        target: PaneId,
        direction: Direction,
        new: PaneId,
    ) -> bool {
        let Some(rect) = self.rect_of(area, target) else { return false };
        if !fits(rect, direction.axis()) {
            return false;
        }
        let axis = direction.axis();
        replace_leaf(&mut self.root, target, |leaf| {
            let (first, second) = if direction.leads() {
                (Box::new(Node::Leaf(new)), Box::new(leaf))
            } else {
                (Box::new(leaf), Box::new(Node::Leaf(new)))
            };
            Node::Split { axis, ratio: 0.5, first, second }
        })
    }

    /// Removes a pane. Returns which pane should take focus, or `None` when
    /// that was the last one and the window itself should close.
    pub fn close(&mut self, area: CellRect, id: PaneId) -> Option<PaneId> {
        if self.len() == 1 {
            return None;
        }
        // The sibling inherits the space, and focus goes to whichever of its
        // panes is nearest to where the closed one was — not to "the first",
        // which after a few splits is somewhere across the window.
        let gone = self.rect_of(area, id);
        if !remove_leaf(&mut self.root, id) {
            return None;
        }
        let (col, row) = gone.map(|r| r.centre()).unwrap_or((0.0, 0.0));
        self.nearest(area, col, row)
    }

    /// Every pane and the cells it occupies, dividers already subtracted.
    pub fn rects(&self, area: CellRect) -> Vec<(PaneId, CellRect)> {
        let mut out = Vec::new();
        layout(&self.root, area, &mut out, &mut Vec::new());
        out
    }

    /// Every divider, for the renderer to draw a line down.
    pub fn dividers(&self, area: CellRect) -> Vec<Divider> {
        let mut rules = Vec::new();
        layout(&self.root, area, &mut Vec::new(), &mut rules);
        rules
    }

    pub fn rect_of(&self, area: CellRect, id: PaneId) -> Option<CellRect> {
        self.rects(area).into_iter().find(|(p, _)| *p == id).map(|(_, r)| r)
    }

    pub fn at(&self, area: CellRect, col: u16, row: u16) -> Option<PaneId> {
        self.rects(area).into_iter().find(|(_, r)| r.contains(col, row)).map(|(p, _)| p)
    }

    /// The pane a directional focus move should land on.
    ///
    /// The neighbour is the closest pane whose rectangle lies in that
    /// direction, measured centre to centre with the crossing distance broken
    /// as a tie — so ⌥⌘→ out of a tall pane into a stack of three lands on the
    /// one you were looking at, not on the top one.
    pub fn neighbour(&self, area: CellRect, from: PaneId, direction: Direction) -> Option<PaneId> {
        let here = self.rect_of(area, from)?;
        let (hx, hy) = here.centre();
        self.rects(area)
            .into_iter()
            .filter(|(id, _)| *id != from)
            .filter_map(|(id, rect)| {
                let (x, y) = rect.centre();
                let (along, across) = match direction {
                    Direction::Left => (hx - x, (y - hy).abs()),
                    Direction::Right => (x - hx, (y - hy).abs()),
                    Direction::Up => (hy - y, (x - hx).abs()),
                    Direction::Down => (y - hy, (x - hx).abs()),
                };
                (along > 0.0).then_some((id, along, across))
            })
            .min_by(|a, b| {
                (a.2, a.1).partial_cmp(&(b.2, b.1)).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(id, _, _)| id)
    }

    fn nearest(&self, area: CellRect, col: f32, row: f32) -> Option<PaneId> {
        self.rects(area)
            .into_iter()
            .min_by(|a, b| {
                let d = |r: &CellRect| {
                    let (x, y) = r.centre();
                    (x - col).powi(2) + (y - row).powi(2)
                };
                d(&a.1).partial_cmp(&d(&b.1)).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(id, _)| id)
    }
}

fn fits(rect: CellRect, axis: Axis) -> bool {
    match axis {
        Axis::Columns => rect.cols >= MIN_COLS * 2 + DIVIDER,
        Axis::Rows => rect.rows >= MIN_ROWS * 2 + DIVIDER,
    }
}

fn collect(node: &Node, out: &mut Vec<PaneId>) {
    match node {
        Node::Leaf(id) => out.push(*id),
        Node::Split { first, second, .. } => {
            collect(first, out);
            collect(second, out);
        }
    }
}

/// Swaps a leaf for whatever `make` builds out of it. Returns false when the
/// leaf is not in this tree, so the caller can report rather than assume.
fn replace_leaf(node: &mut Node, id: PaneId, make: impl FnOnce(Node) -> Node) -> bool {
    match node {
        Node::Leaf(here) if *here == id => {
            let leaf = Node::Leaf(*here);
            *node = make(leaf);
            true
        }
        Node::Leaf(_) => false,
        Node::Split { first, second, .. } => {
            // `make` is FnOnce, so it can only be handed to one branch — hence
            // the explicit test rather than `||`, which would move it twice.
            if contains(first, id) {
                replace_leaf(first, id, make)
            } else {
                replace_leaf(second, id, make)
            }
        }
    }
}

fn contains(node: &Node, id: PaneId) -> bool {
    match node {
        Node::Leaf(here) => *here == id,
        Node::Split { first, second, .. } => contains(first, id) || contains(second, id),
    }
}

/// Collapses the split that held `id`, promoting its sibling.
fn remove_leaf(node: &mut Node, id: PaneId) -> bool {
    let Node::Split { first, second, .. } = node else { return false };
    if **first == Node::Leaf(id) {
        *node = (**second).clone();
        return true;
    }
    if **second == Node::Leaf(id) {
        *node = (**first).clone();
        return true;
    }
    remove_leaf(first, id) || remove_leaf(second, id)
}

fn layout(node: &Node, area: CellRect, out: &mut Vec<(PaneId, CellRect)>, rules: &mut Vec<Divider>) {
    match node {
        Node::Leaf(id) => out.push((*id, area)),
        Node::Split { axis, ratio, first, second } => match axis {
            Axis::Columns => {
                let usable = area.cols.saturating_sub(DIVIDER);
                let left = share(usable, *ratio, MIN_COLS);
                let right = usable.saturating_sub(left);
                rules.push(Divider {
                    axis: Axis::Columns,
                    at: area.col + left,
                    start: area.row,
                    len: area.rows,
                });
                layout(first, CellRect::new(area.col, area.row, left, area.rows), out, rules);
                layout(
                    second,
                    CellRect::new(area.col + left + DIVIDER, area.row, right, area.rows),
                    out,
                    rules,
                );
            }
            Axis::Rows => {
                let usable = area.rows.saturating_sub(DIVIDER);
                let top = share(usable, *ratio, MIN_ROWS);
                let bottom = usable.saturating_sub(top);
                rules.push(Divider {
                    axis: Axis::Rows,
                    at: area.row + top,
                    start: area.col,
                    len: area.cols,
                });
                layout(first, CellRect::new(area.col, area.row, area.cols, top), out, rules);
                layout(
                    second,
                    CellRect::new(area.col, area.row + top + DIVIDER, area.cols, bottom),
                    out,
                    rules,
                );
            }
        },
    }
}

/// The first child's share of `total`, kept inside the minimum at both ends.
///
/// Rounding is `round`, not truncation: an even split of an odd number should
/// give the extra cell to one side rather than dropping it, and truncation
/// drops it on every level of a deep tree.
fn share(total: u16, ratio: f32, min: u16) -> u16 {
    if total <= min {
        return total;
    }
    let raw = (total as f32 * ratio.clamp(0.0, 1.0)).round() as u16;
    raw.clamp(min, total.saturating_sub(min))
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: PaneId = PaneId(1);
    const B: PaneId = PaneId(2);
    const C: PaneId = PaneId(3);
    const D: PaneId = PaneId(4);

    fn area() -> CellRect {
        CellRect::new(0, 0, 118, 34)
    }

    #[test]
    fn one_pane_owns_the_whole_window() {
        let layout = Layout::new(A);
        assert_eq!(layout.rects(area()), vec![(A, area())]);
        assert!(layout.dividers(area()).is_empty(), "a single pane needs no rule");
    }

    #[test]
    fn a_vertical_split_gives_the_divider_a_cell_of_its_own() {
        // The cell the rule sits in belongs to neither pane. If it did, one of
        // them would draw text under the line.
        let mut layout = Layout::new(A);
        assert!(layout.split(area(), A, Direction::Right, B));

        let rects = layout.rects(area());
        let a = rects.iter().find(|(id, _)| *id == A).unwrap().1;
        let b = rects.iter().find(|(id, _)| *id == B).unwrap().1;
        assert_eq!(a.cols + b.cols + DIVIDER, area().cols);
        assert_eq!(b.col, a.col + a.cols + DIVIDER);
        assert_eq!(a.rows, area().rows);

        let rules = layout.dividers(area());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].axis, Axis::Columns);
        assert_eq!(rules[0].at, a.cols);
        assert_eq!(rules[0].len, area().rows);
    }

    #[test]
    fn direction_decides_which_side_the_new_pane_lands_on() {
        // Splitting left has to put the new pane on the left. Getting this
        // backwards is invisible in a test that only counts panes.
        let mut right = Layout::new(A);
        right.split(area(), A, Direction::Right, B);
        assert!(right.rect_of(area(), B).unwrap().col > right.rect_of(area(), A).unwrap().col);

        let mut left = Layout::new(A);
        left.split(area(), A, Direction::Left, B);
        assert!(left.rect_of(area(), B).unwrap().col < left.rect_of(area(), A).unwrap().col);

        let mut down = Layout::new(A);
        down.split(area(), A, Direction::Down, B);
        assert!(down.rect_of(area(), B).unwrap().row > down.rect_of(area(), A).unwrap().row);

        let mut up = Layout::new(A);
        up.split(area(), A, Direction::Up, B);
        assert!(up.rect_of(area(), B).unwrap().row < up.rect_of(area(), A).unwrap().row);
    }

    #[test]
    fn panes_never_overlap_and_never_leave_the_window() {
        let mut layout = Layout::new(A);
        layout.split(area(), A, Direction::Right, B);
        layout.split(area(), B, Direction::Down, C);
        layout.split(area(), A, Direction::Down, D);

        let rects = layout.rects(area());
        assert_eq!(rects.len(), 4);
        for (_, r) in &rects {
            assert!(r.col + r.cols <= area().cols, "{r:?} runs off the right");
            assert!(r.row + r.rows <= area().rows, "{r:?} runs off the bottom");
            assert!(r.cols >= MIN_COLS && r.rows >= MIN_ROWS, "{r:?} is too small to use");
        }
        for (i, (_, a)) in rects.iter().enumerate() {
            for (_, b) in rects.iter().skip(i + 1) {
                let apart = a.col + a.cols <= b.col
                    || b.col + b.cols <= a.col
                    || a.row + a.rows <= b.row
                    || b.row + b.rows <= a.row;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_split_that_would_not_fit_is_refused_rather_than_made() {
        // A two-column pane is not a pane. Refusing leaves the tree exactly as
        // it was, so the caller can say so and nothing is half-applied.
        let narrow = CellRect::new(0, 0, MIN_COLS * 2, 34);
        let mut layout = Layout::new(A);
        let before = layout.clone();
        assert!(!layout.split(narrow, A, Direction::Right, B));
        assert_eq!(layout, before);

        let short = CellRect::new(0, 0, 118, MIN_ROWS * 2);
        assert!(!layout.split(short, A, Direction::Down, B));
        assert_eq!(layout, before);
    }

    #[test]
    fn splitting_a_pane_that_is_not_there_changes_nothing() {
        let mut layout = Layout::new(A);
        let before = layout.clone();
        assert!(!layout.split(area(), C, Direction::Right, B));
        assert_eq!(layout, before);
    }

    #[test]
    fn closing_a_pane_gives_its_space_to_the_sibling() {
        let mut layout = Layout::new(A);
        layout.split(area(), A, Direction::Right, B);
        assert_eq!(layout.close(area(), B), Some(A));
        assert_eq!(layout.rects(area()), vec![(A, area())], "the survivor did not take the space");
    }

    #[test]
    fn closing_the_last_pane_asks_the_window_to_close() {
        let mut layout = Layout::new(A);
        assert_eq!(layout.close(area(), A), None);
    }

    #[test]
    fn focus_after_a_close_goes_to_the_nearest_pane_not_the_first() {
        // Three in a row; close the right-hand one and focus should step left
        // by one, not jump to the far edge.
        let mut layout = Layout::new(A);
        layout.split(area(), A, Direction::Right, B);
        layout.split(area(), B, Direction::Right, C);
        assert_eq!(layout.close(area(), C), Some(B));
    }

    #[test]
    fn a_directional_move_lands_on_the_pane_you_were_looking_at() {
        // A tall pane on the left, a stack of two on the right. Moving right
        // from the left pane must not always mean "the top one" — it means the
        // one across from where you are.
        let mut layout = Layout::new(A);
        layout.split(area(), A, Direction::Right, B);
        layout.split(area(), B, Direction::Down, C);

        assert_eq!(layout.neighbour(area(), A, Direction::Right), Some(B));
        assert_eq!(layout.neighbour(area(), B, Direction::Left), Some(A));
        assert_eq!(layout.neighbour(area(), B, Direction::Down), Some(C));
        assert_eq!(layout.neighbour(area(), C, Direction::Up), Some(B));
        assert_eq!(layout.neighbour(area(), A, Direction::Left), None, "there is nothing further left");
    }

    #[test]
    fn a_click_finds_the_pane_under_it() {
        let mut layout = Layout::new(A);
        layout.split(area(), A, Direction::Right, B);
        let a = layout.rect_of(area(), A).unwrap();

        assert_eq!(layout.at(area(), 0, 0), Some(A));
        assert_eq!(layout.at(area(), a.cols + DIVIDER, 0), Some(B));
        // The divider itself belongs to nobody.
        assert_eq!(layout.at(area(), a.cols, 0), None);
        assert_eq!(layout.at(area(), area().cols, 0), None, "off the right edge");
    }

    #[test]
    fn every_cell_of_the_window_is_either_a_pane_or_a_divider() {
        // The strongest statement of the layout's job: nothing is unaccounted
        // for, so no gap can open between two panes and go unnoticed.
        let mut layout = Layout::new(A);
        layout.split(area(), A, Direction::Right, B);
        layout.split(area(), B, Direction::Down, C);
        layout.split(area(), A, Direction::Down, D);

        let rects = layout.rects(area());
        let rules = layout.dividers(area());
        for row in 0..area().rows {
            for col in 0..area().cols {
                let in_pane = rects.iter().any(|(_, r)| r.contains(col, row));
                let on_rule = rules.iter().any(|d| match d.axis {
                    Axis::Columns => col == d.at && row >= d.start && row < d.start + d.len,
                    Axis::Rows => row == d.at && col >= d.start && col < d.start + d.len,
                });
                assert!(in_pane || on_rule, "cell ({col}, {row}) belongs to nothing");
                assert!(!(in_pane && on_rule), "cell ({col}, {row}) is both pane and rule");
            }
        }
    }

    #[test]
    fn a_pane_keeps_its_identity_when_its_neighbour_goes_away() {
        // Focus follows the pane, not the position. If ids were reassigned on
        // close, the focused pane would silently become a different shell.
        let mut layout = Layout::new(A);
        layout.split(area(), A, Direction::Right, B);
        layout.split(area(), A, Direction::Down, C);
        layout.close(area(), C);
        assert_eq!(layout.panes(), vec![A, B]);
    }
}
