//! The parts of a mask: a view on the stored mask tree, no format of its own.
//!
//! A mask is stored as a tree (`Combine { operation, left, right }`, `Invert`). The list of parts follows the left
//! spine of that tree: `Combine(op_n, … Combine(op_1, base, part_1) …, part_n)` is `[base, (op_1, part_1), …,
//! (op_n, part_n)]`, an `Invert` around the whole tree is the switch "mask inverted". A part is a leaf, an inverted
//! leaf, a brush part (an add-tree of brush strokes, drawn into as one part) or any other subtree, shown as one part
//! "composed". [`split`] and [`join`] are exact inverses on every tree, so showing the parts never changes a stored
//! mask; only an edit of a part rebuilds the tree, and only along the spine.
use masks::{Mask, Operation};

/// The mask as a list of parts.
#[derive(Clone, Debug, PartialEq)]
pub struct Parts {
    /// `Invert` around the whole tree
    pub inverted: bool,
    /// the first part (operation `None`) and the following parts with their operation
    pub parts: Vec<(Option<Operation>, Mask)>,
}

/// Is `m` a brush part: one brush stroke or an add-tree of brush strokes (also inverted)?
pub fn is_brush(m: &Mask) -> bool {
    match m {
        Mask::Brush { .. } => true,
        Mask::Invert { mask } => matches!(**mask, Mask::Brush { .. } | Mask::Combine { operation: Operation::Add, .. }) && is_brush(mask),
        Mask::Combine { operation: Operation::Add, left, right } => strokes_only(left) && strokes_only(right),
        _ => false,
    }
}

fn strokes_only(m: &Mask) -> bool {
    match m {
        Mask::Brush { .. } => true,
        Mask::Combine { operation: Operation::Add, left, right } => strokes_only(left) && strokes_only(right),
        _ => false,
    }
}

/// Number of brush strokes of a brush part.
pub fn strokes(m: &Mask) -> usize {
    match m {
        Mask::Brush { points, .. } => usize::from(!fresh(points)),
        Mask::Combine { left, right, .. } => strokes(left) + strokes(right),
        Mask::Invert { mask } => strokes(mask),
        _ => 0,
    }
}

/// A brush that has not been drawn yet (placeholder point outside the photo).
pub fn fresh(points: &[[f32; 2]]) -> bool {
    points.iter().all(|p| p[0] < 0.0 || p[1] < 0.0)
}

/// The parts of a mask (see the module comment).
pub fn split(mask: &Mask) -> Parts {
    // an inverted brush part at the root stays one inverted part (as drawn so far)
    let (inverted, tree) = match mask {
        Mask::Invert { mask: inner } if !is_brush(mask) => (true, &**inner),
        m => (false, m),
    };
    let mut parts = Vec::new();
    spine(tree, &mut parts);
    Parts { inverted, parts }
}

fn spine(m: &Mask, out: &mut Vec<(Option<Operation>, Mask)>) {
    match m {
        Mask::Combine { operation, left, right } if !is_brush(m) => {
            spine(left, out);
            out.push((Some(*operation), (**right).clone()));
        }
        m => out.push((None, m.clone())),
    }
}

/// Is the whole mask inverted (an `Invert` around a tree that is not itself one brush part)?
pub fn inverted(mask: &Mask) -> bool {
    matches!(mask, Mask::Invert { .. }) && !is_brush(mask)
}

fn tree(mask: &Mask) -> &Mask {
    match mask {
        Mask::Invert { mask: inner } if !is_brush(mask) => inner,
        m => m,
    }
}

fn tree_mut(mask: &mut Mask) -> &mut Mask {
    if inverted(mask) {
        match mask {
            Mask::Invert { mask } => mask,
            _ => unreachable!(),
        }
    } else {
        mask
    }
}

fn spine_len(m: &Mask) -> usize {
    match m {
        Mask::Combine { left, .. } if !is_brush(m) => spine_len(left) + 1,
        _ => 1,
    }
}

/// Number of parts of a mask (without cloning it, see [`split`]).
pub fn count(mask: &Mask) -> usize {
    spine_len(tree(mask))
}

/// Part `i` of a mask, in place (no copy of the tree): what drawing in the image and the controls of a part change.
pub fn part_mut(mask: &mut Mask, i: usize) -> Option<&mut Mask> {
    fn get(node: &mut Mask, i: usize) -> Option<&mut Mask> {
        let n = spine_len(node);
        if i >= n {
            return None;
        }
        if n == 1 {
            return Some(node);
        }
        match node {
            Mask::Combine { left, right, .. } => {
                if i == n - 1 {
                    Some(right)
                } else {
                    get(left, i)
                }
            }
            _ => None,
        }
    }
    get(tree_mut(mask), i)
}

/// Operations of the parts (the first one has none), without cloning the tree.
pub fn operations(mask: &Mask) -> Vec<Option<Operation>> {
    fn walk(m: &Mask, out: &mut Vec<Option<Operation>>) {
        match m {
            Mask::Combine { operation, left, .. } if !is_brush(m) => {
                walk(left, out);
                out.push(Some(*operation));
            }
            _ => out.push(None),
        }
    }
    let mut out = Vec::new();
    walk(tree(mask), &mut out);
    out
}

/// Part `i` of a mask, read only.
pub fn part(mask: &Mask, i: usize) -> Option<&Mask> {
    fn get(node: &Mask, i: usize) -> Option<&Mask> {
        let n = spine_len(node);
        if i >= n {
            return None;
        }
        if n == 1 {
            return Some(node);
        }
        match node {
            Mask::Combine { left, right, .. } => {
                if i == n - 1 {
                    Some(right)
                } else {
                    get(left, i)
                }
            }
            _ => None,
        }
    }
    get(tree(mask), i)
}

/// The mask tree of a list of parts: folded from the left, inverted as a whole when switched so.
pub fn join(p: &Parts) -> Mask {
    let mut it = p.parts.iter();
    let mut tree = it.next().map(|(_, m)| m.clone()).unwrap_or(Mask::Brush { points: vec![[-2.0, -2.0]], radius: 0.04, hardness: 0.6, flow: 0.2 });
    for (op, m) in it {
        tree = Mask::Combine { operation: op.unwrap_or(Operation::Add), left: Box::new(tree), right: Box::new(m.clone()) };
    }
    if p.inverted {
        Mask::Invert { mask: Box::new(tree) }
    } else {
        tree
    }
}

impl Parts {
    /// Add a part at the end with `op`; `false` when the tree would get too large or deep (nothing changed).
    pub fn push(&mut self, op: Operation, m: Mask) -> bool {
        let mut q = self.clone();
        q.parts.push((Some(op), m));
        if join(&q).validate().is_err() {
            return false;
        }
        *self = q;
        true
    }

    /// Can part `i` be removed? Not the only part (that is removing the mask), and not the first one while the next
    /// one subtracts or intersects (without its base it would act on the whole photo).
    pub fn can_remove(&self, i: usize) -> bool {
        self.parts.len() > 1 && i < self.parts.len() && (i > 0 || self.parts[1].0 == Some(Operation::Add))
    }

    /// Remove part `i` (see [`Self::can_remove`]); the next part becomes the first one.
    pub fn remove(&mut self, i: usize) -> bool {
        if !self.can_remove(i) {
            return false;
        }
        self.parts.remove(i);
        self.parts[0].0 = None;
        true
    }

    /// Change the operation of part `i` (not the first one).
    pub fn set_operation(&mut self, i: usize, op: Operation) -> bool {
        match self.parts.get_mut(i) {
            Some(p) if i > 0 => {
                p.0 = Some(op);
                true
            }
            _ => false,
        }
    }

    /// Invert part `i` (or undo the inversion).
    pub fn invert(&mut self, i: usize) {
        if let Some(p) = self.parts.get_mut(i) {
            p.1 = match std::mem::replace(&mut p.1, Mask::Linear { start: [0.0; 2], end: [1.0; 2] }) {
                Mask::Invert { mask } => *mask,
                m => Mask::Invert { mask: Box::new(m) },
            };
        }
    }
}

/// A new stroke with the brush settings `brush` (radius, hardness, flow) at the end of the brush part `part`: a part
/// that is still a placeholder is drawn into directly, otherwise the strokes are kept in a balanced add-tree (an
/// inverted part stays inverted). The stroke to draw into is the last leaf of the result ([`last_leaf`]).
pub fn add_stroke(part: &Mask, brush: [f32; 3]) -> Mask {
    fn collect(m: &Mask, out: &mut Vec<Mask>) {
        match m {
            Mask::Combine { operation: Operation::Add, left, right } => {
                collect(left, out);
                collect(right, out);
            }
            m => out.push(m.clone()),
        }
    }
    fn balance(v: &[Mask]) -> Mask {
        if v.len() == 1 {
            return v[0].clone();
        }
        let n = v.len() / 2;
        Mask::Combine { operation: Operation::Add, left: Box::new(balance(&v[..n])), right: Box::new(balance(&v[n..])) }
    }
    let [radius, hardness, flow] = brush;
    let new = Mask::Brush { points: vec![[-2.0, -2.0]], radius, hardness, flow };
    match part {
        Mask::Invert { mask } => Mask::Invert { mask: Box::new(add_stroke(mask, brush)) },
        Mask::Brush { points, .. } if fresh(points) => new,
        m => {
            let mut strokes = Vec::new();
            collect(m, &mut strokes);
            strokes.push(new);
            balance(&strokes)
        }
    }
}

/// The leaf of a part that drawing and the shape controls act on: the last stroke of a brush part, the leaf of an
/// inverted leaf, the right-most leaf otherwise.
pub fn last_leaf(m: &mut Mask) -> &mut Mask {
    match m {
        Mask::Invert { mask } => last_leaf(mask),
        Mask::Combine { right, .. } => last_leaf(right),
        m => m,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use masks::{Instances, RasterSource};

    fn brush(x: f32) -> Mask {
        Mask::Brush { points: vec![[x, 0.5], [x + 0.1, 0.55]], radius: 0.05, hardness: 0.5, flow: 0.8 }
    }
    fn subject() -> Mask {
        Mask::Raster { raster: String::new(), source: RasterSource::Subject { instances: Instances::All }, feather: 0.0, range: None, edge: 0.0 }
    }
    fn radial() -> Mask {
        Mask::Radial { center: [0.5, 0.5], radii: [0.3, 0.2], angle: 0.0, feather: 0.5 }
    }
    fn trees() -> Vec<Mask> {
        let c = |op, l: Mask, r: Mask| Mask::Combine { operation: op, left: Box::new(l), right: Box::new(r) };
        let inv = |m: Mask| Mask::Invert { mask: Box::new(m) };
        vec![
            radial(),
            brush(0.1),
            c(Operation::Add, brush(0.1), brush(0.3)),
            c(Operation::Add, c(Operation::Add, brush(0.1), brush(0.2)), c(Operation::Add, brush(0.3), brush(0.4))),
            c(Operation::Subtract, subject(), c(Operation::Add, brush(0.1), brush(0.2))),
            c(Operation::Add, c(Operation::Add, subject(), brush(0.1)), c(Operation::Add, brush(0.2), brush(0.3))),
            c(Operation::Intersect, c(Operation::Subtract, radial(), inv(brush(0.2))), Mask::Luminance { low: 0.1, high: 0.6, feather: 0.1 }),
            inv(c(Operation::Subtract, radial(), brush(0.2))),
            inv(brush(0.4)),
            inv(c(Operation::Add, brush(0.1), brush(0.3))),
            inv(inv(radial())),
            // not left-deep: the right side is a composed part
            c(Operation::Subtract, radial(), c(Operation::Intersect, subject(), radial())),
        ]
    }

    /// Showing the parts never changes the stored tree (also trees not built by this view).
    #[test]
    fn split_and_join_give_back_the_same_tree() {
        for t in trees() {
            let p = split(&t);
            assert_eq!(join(&p), t, "{p:?}");
            assert_eq!(count(&t), p.parts.len());
            assert_eq!(inverted(&t), p.inverted);
            let mut m = t.clone();
            for (i, (_, x)) in p.parts.iter().enumerate() {
                assert_eq!(part(&t, i), Some(x));
                assert_eq!(part_mut(&mut m, i).map(|x| x.clone()).as_ref(), Some(x));
            }
            assert!(part(&t, p.parts.len()).is_none());
            assert!(p.parts[0].0.is_none());
            assert!(p.parts[1..].iter().all(|x| x.0.is_some()));
        }
    }

    #[test]
    fn brush_parts_stay_one_part() {
        let c = |op, l: Mask, r: Mask| Mask::Combine { operation: op, left: Box::new(l), right: Box::new(r) };
        let p = split(&c(Operation::Subtract, subject(), c(Operation::Add, brush(0.1), brush(0.2))));
        assert_eq!(p.parts.len(), 2);
        assert_eq!(strokes(&p.parts[1].1), 2);
        let p = split(&c(Operation::Add, c(Operation::Add, brush(0.1), brush(0.2)), c(Operation::Add, brush(0.3), brush(0.4))));
        assert_eq!(p.parts.len(), 1);
        assert_eq!(strokes(&p.parts[0].1), 4);
        assert!(!split(&Mask::Invert { mask: Box::new(brush(0.1)) }).inverted);
    }

    #[test]
    fn new_strokes_go_into_the_chosen_part_only() {
        let c = |op, l: Mask, r: Mask| Mask::Combine { operation: op, left: Box::new(l), right: Box::new(r) };
        let tree = c(Operation::Subtract, c(Operation::Add, subject(), brush(0.1)), brush(0.6));
        let mut p = split(&tree);
        assert_eq!(p.parts.len(), 3);
        p.parts[2].1 = add_stroke(&p.parts[2].1, [0.05, 0.5, 0.8]);
        if let Mask::Brush { points, .. } = last_leaf(&mut p.parts[2].1) {
            assert!(fresh(points));
            *points = vec![[0.7, 0.7]];
        }
        assert_eq!(strokes(&p.parts[1].1), 1);
        assert_eq!(strokes(&p.parts[2].1), 2);
        let t = join(&p);
        t.validate().unwrap();
        // the first two parts are untouched
        assert_eq!(split(&t).parts[..2], split(&tree).parts[..2]);
        // a placeholder is drawn into, not doubled
        let placeholder = Mask::Brush { points: vec![[-2.0, -2.0]], radius: 0.05, hardness: 0.5, flow: 0.8 };
        assert_eq!(add_stroke(&placeholder, [0.05, 0.5, 0.8]), placeholder);
    }

    #[test]
    fn removing_and_operations() {
        let mut p = Parts { inverted: false, parts: vec![(None, subject())] };
        assert!(!p.can_remove(0));
        assert!(p.push(Operation::Subtract, brush(0.2)));
        assert!(!p.can_remove(0), "without its base a subtraction would act on the whole photo");
        assert!(p.can_remove(1));
        assert!(p.set_operation(1, Operation::Add));
        assert!(!p.set_operation(0, Operation::Add));
        assert!(p.remove(0));
        assert_eq!(p.parts, vec![(None, brush(0.2))]);
        p.invert(0);
        assert!(matches!(p.parts[0].1, Mask::Invert { .. }));
        p.invert(0);
        assert_eq!(p.parts[0].1, brush(0.2));
    }

    #[test]
    fn too_deep_trees_are_refused() {
        let mut p = Parts { inverted: false, parts: vec![(None, radial())] };
        let mut n = 0;
        while p.push(Operation::Add, radial()) {
            n += 1;
            assert!(n < 100);
        }
        join(&p).validate().unwrap();
        assert!(n >= 30);
    }
}
