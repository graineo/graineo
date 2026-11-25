//! One measured width rule for the bar below the image (Develop and Darkroom): groups in a fixed rank;
//! when the space runs short the lowest groups move completely into the menu "MEHR", before anything touches.

/// Groups of the bar. The order of the variants is the rank in which they give way (first = first to go).
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Group {
    Labels,
    Flag,
    Stars,
    Darkroom,
    Rotate,
    Overlays,
    ZoomButtons,
    View,
}

impl Group {
    pub fn key(self) -> &'static str {
        match self {
            Group::Labels => "labels",
            Group::Flag => "flag",
            Group::Stars => "stars",
            Group::Darkroom => "darkroom",
            Group::Rotate => "rotate",
            Group::Overlays => "overlays",
            Group::ZoomButtons => "zoom-buttons",
            Group::View => "view",
        }
    }
}

/// Fixed distance between groups.
pub const GAP: f32 = 10.0;
/// Width of the "MEHR" button.
pub const MORE_W: f32 = 58.0;

/// `fixed`: width of the groups that always stay (view, zoom, help, condition mark);
/// `groups`: the others with their measured widths. Returns the groups that stay visible.
pub fn plan(available: f32, fixed: &[f32], groups: &[(Group, f32)]) -> Vec<Group> {
    let width = |set: &[(Group, f32)], more: bool| {
        let n = fixed.len() + set.len() + usize::from(more);
        fixed.iter().sum::<f32>() + set.iter().map(|g| g.1).sum::<f32>() + if more { MORE_W } else { 0.0 } + GAP * n.saturating_sub(1) as f32
    };
    let mut shown: Vec<(Group, f32)> = groups.to_vec();
    shown.sort_by_key(|g| g.0);
    while !shown.is_empty() && width(&shown, shown.len() < groups.len()) > available {
        shown.remove(0);
    }
    let mut v: Vec<Group> = shown.iter().map(|g| g.0).collect();
    v.sort();
    v
}

/// Total width the plan uses (for the tests).
pub fn used(available: f32, fixed: &[f32], groups: &[(Group, f32)]) -> f32 {
    let shown = plan(available, fixed, groups);
    let more = shown.len() < groups.len();
    let n = fixed.len() + shown.len() + usize::from(more);
    fixed.iter().sum::<f32>() + groups.iter().filter(|g| shown.contains(&g.0)).map(|g| g.1).sum::<f32>() + if more { MORE_W } else { 0.0 } + GAP * n.saturating_sub(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_never_overlap_and_go_whole() {
        let fixed = [140.0];
        let groups = [(Group::View, 120.0), (Group::ZoomButtons, 60.0), (Group::Flag, 60.0), (Group::Stars, 110.0), (Group::Labels, 110.0), (Group::Rotate, 60.0), (Group::Overlays, 180.0), (Group::Darkroom, 200.0)];
        let mut w = 200.0;
        let mut last = 0;
        while w <= 1600.0 {
            let shown = plan(w, &fixed, &groups);
            assert!(shown.len() >= last, "more room never hides more groups ({w})");
            last = shown.len();
            if shown.len() > 0 || used(w, &fixed, &groups) <= w {
                assert!(used(w, &fixed, &groups) <= w + 0.01, "overlap at {w} pt");
            }
            w += 20.0;
        }
        assert_eq!(plan(2000.0, &fixed, &groups).len(), groups.len());
        // the lowest rank goes first
        let p = plan(900.0, &fixed, &groups);
        assert!(!p.contains(&Group::Labels) || p.len() == groups.len());
    }
}
