//! Model of the people view, without drawing: filter columns (person, status, source, year) with counts, and the
//! result as sections of tiles. Works only on the face index in memory ([`library::face_index`]); a rebuild over
//! 110 000 faces takes a few milliseconds, so it happens right in the frame of a click.

use std::collections::{BTreeSet, HashMap};

use library::face_index::{FaceIndex, Source, Status, ALL_SOURCES, ALL_STATUS};

/// Person column: value of "(ohne Namen)".
pub const NO_PERSON: i64 = -1;

/// What the filter columns have selected (empty = no restriction; status empty = the standard set).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub people: BTreeSet<i64>,
    pub status: BTreeSet<Status>,
    pub sources: BTreeSet<Source>,
    pub years: BTreeSet<u16>,
}

/// Status shown when nothing is selected in the status column (unnamed single faces and "do not name" only on request).
pub fn standard_status(s: Status) -> bool {
    !matches!(s, Status::Unnamed | Status::Rejected)
}

impl Filter {
    fn status_ok(&self, s: Status) -> bool {
        if self.status.is_empty() {
            standard_status(s)
        } else {
            self.status.contains(&s)
        }
    }

    /// Exactly one person chosen (search or column)?
    pub fn one_person(&self) -> Option<i64> {
        (self.people.len() == 1).then(|| *self.people.iter().next().unwrap()).filter(|p| *p != NO_PERSON)
    }
}

/// An opened stack: all confirmed faces of a person, or the faces of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stack {
    Person(i64),
    Group(i64),
}

/// What a tile shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// a single face
    Face(i64),
    /// the confirmed faces of a person as one stack
    Person(i64),
    /// a group of similar unnamed faces as one stack
    Group(i64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tile {
    pub kind: Kind,
    /// face shown as image
    pub face: Option<i64>,
    /// stacks: number of faces
    pub count: usize,
}

/// Kind of a section (its title is made when drawing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    /// people as stacks (one person chosen: its stack)
    People,
    Questions,
    Doubtful,
    Groups,
    Unnamed,
    Rejected,
    /// inside an opened stack
    StackFaces,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    pub kind: SectionKind,
    pub range: std::ops::Range<usize>,
}

/// A row of a filter column: value, count of confirmed and of open questions (person) or of faces (other columns).
#[derive(Clone, Debug, PartialEq)]
pub struct Count {
    pub value: i64,
    pub n: usize,
    /// person column: open questions (suggestions and doubts naming this person)
    pub open: usize,
}

/// Result of a rebuild.
#[derive(Clone, Debug, Default)]
pub struct Built {
    pub tiles: Vec<Tile>,
    pub sections: Vec<Section>,
    /// alphabetically, "(ohne Namen)" last
    pub people: Vec<Count>,
    /// in the order of [`ALL_STATUS`]
    pub status: Vec<Count>,
    pub sources: Vec<Count>,
    /// newest first, unknown (0) last
    pub years: Vec<Count>,
    /// faces of suggestions that are very likely in the current result (for "confirm all very likely")
    pub sure: Vec<i64>,
}

/// Does a face match the person selection? Doubtful faces also count for the person they look like.
fn person_ok(f: &library::face_index::Face, people: &BTreeSet<i64>) -> bool {
    if people.is_empty() {
        return true;
    }
    match f.person {
        Some(p) => people.contains(&p) || f.suspect.is_some_and(|s| people.contains(&s)),
        None => people.contains(&NO_PERSON),
    }
}

/// Filter, count and arrange. `show_hidden`: stacks of hidden people also without choosing them.
pub fn build(ix: &FaceIndex, filter: &Filter, stack: Option<Stack>) -> Built {
    let mut b = Built::default();
    // ── counts: each column counts the faces that pass all *other* columns
    let mut per_person: HashMap<i64, (usize, usize)> = HashMap::new();
    let mut per_status: HashMap<Status, usize> = HashMap::new();
    let mut per_source: HashMap<Source, usize> = HashMap::new();
    let mut per_year: HashMap<u16, usize> = HashMap::new();
    for f in &ix.faces {
        let p = person_ok(f, &filter.people);
        let s = filter.status_ok(f.status);
        let q = filter.sources.is_empty() || filter.sources.contains(&f.source);
        let y = filter.years.is_empty() || filter.years.contains(&f.year);
        if q && y {
            // person column: confirmed · open, regardless of the status column
            match f.status {
                Status::Confirmed => per_person.entry(f.person.unwrap_or(NO_PERSON)).or_default().0 += 1,
                Status::SuggestSure | Status::SuggestMaybe => per_person.entry(f.person.unwrap_or(NO_PERSON)).or_default().1 += 1,
                Status::Doubtful => {
                    per_person.entry(f.person.unwrap_or(NO_PERSON)).or_default().0 += 1;
                    if let Some(x) = f.suspect {
                        per_person.entry(x).or_default().1 += 1;
                    }
                }
                Status::UnnamedGroup | Status::Unnamed => per_person.entry(NO_PERSON).or_default().0 += 1,
                Status::Rejected => {}
            }
        }
        if p && q && y {
            *per_status.entry(f.status).or_default() += 1;
        }
        if p && s && y {
            *per_source.entry(f.source).or_default() += 1;
        }
        if p && s && q {
            *per_year.entry(f.year).or_default() += 1;
        }
    }
    b.people = ix.people.iter().map(|x| {
        let (n, open) = per_person.get(&x.id).copied().unwrap_or((0, 0));
        Count { value: x.id, n, open }
    }).collect();
    b.people.push(Count { value: NO_PERSON, n: per_person.get(&NO_PERSON).map_or(0, |x| x.0), open: 0 });
    b.status = ALL_STATUS.iter().map(|s| Count { value: *s as i64, n: per_status.get(s).copied().unwrap_or(0), open: 0 }).collect();
    b.sources = ALL_SOURCES.iter().map(|s| Count { value: *s as i64, n: per_source.get(s).copied().unwrap_or(0), open: 0 }).collect();
    let mut years: Vec<u16> = per_year.keys().copied().chain(filter.years.iter().copied()).collect();
    years.sort_unstable_by(|a, b| (*a == 0).cmp(&(*b == 0)).then(b.cmp(a)));
    years.dedup();
    b.years = years.into_iter().map(|y| Count { value: y as i64, n: per_year.get(&y).copied().unwrap_or(0), open: 0 }).collect();

    // ── result
    let pass = |f: &library::face_index::Face| person_ok(f, &filter.people) && (filter.sources.is_empty() || filter.sources.contains(&f.source)) && (filter.years.is_empty() || filter.years.contains(&f.year));
    if let Some(st) = stack {
        let mut faces: Vec<&library::face_index::Face> = ix
            .faces
            .iter()
            .filter(|f| match st {
                Stack::Person(p) => f.person == Some(p) && matches!(f.status, Status::Confirmed | Status::Doubtful),
                Stack::Group(g) => f.group == Some(g) && f.status == Status::UnnamedGroup,
            })
            .filter(|f| (filter.sources.is_empty() || filter.sources.contains(&f.source)) && (filter.years.is_empty() || filter.years.contains(&f.year)))
            .collect();
        match st {
            Stack::Person(_) => faces.sort_by(|a, b| b.capture.cmp(&a.capture).then(b.id.cmp(&a.id))),
            Stack::Group(_) => faces.sort_by(|a, b| b.quality.total_cmp(&a.quality).then(a.id.cmp(&b.id))),
        }
        let start = b.tiles.len();
        b.tiles.extend(faces.iter().map(|f| Tile { kind: Kind::Face(f.id), face: Some(f.id), count: 1 }));
        b.sections.push(Section { kind: SectionKind::StackFaces, range: start..b.tiles.len() });
        return b;
    }
    let hidden: std::collections::HashSet<i64> = ix.people.iter().filter(|p| p.hidden).map(|p| p.id).collect();
    // people as stacks (alphabetically, like the person column)
    if filter.status_ok(Status::Confirmed) {
        let mut stacks: HashMap<i64, (usize, Option<(f32, i64)>)> = HashMap::new();
        // a doubtful face still carries its confirmed name: it counts for the stack (as in the person column)
        for f in ix.faces.iter().filter(|f| matches!(f.status, Status::Confirmed | Status::Doubtful) && pass(f)) {
            let Some(p) = f.person else { continue };
            // a doubtful face of someone else that looks like the chosen person does not bring its stack along
            if (hidden.contains(&p) || !filter.people.is_empty()) && !filter.people.contains(&p) {
                continue;
            }
            let e = stacks.entry(p).or_default();
            e.0 += 1;
            if e.1.is_none_or(|(q, _)| f.quality > q) {
                e.1 = Some((f.quality, f.id));
            }
        }
        let start = b.tiles.len();
        for p in &ix.people {
            if let Some((n, best)) = stacks.get(&p.id) {
                b.tiles.push(Tile { kind: Kind::Person(p.id), face: best.map(|x| x.1), count: *n });
            }
        }
        if b.tiles.len() > start {
            b.sections.push(Section { kind: SectionKind::People, range: start..b.tiles.len() });
        }
    }
    // suggestions: very likely first, then by clarity
    let mut questions: Vec<&library::face_index::Face> = ix.faces.iter().filter(|f| matches!(f.status, Status::SuggestSure | Status::SuggestMaybe) && filter.status_ok(f.status) && pass(f)).collect();
    questions.sort_by(|a, c| (a.status != Status::SuggestSure).cmp(&(c.status != Status::SuggestSure)).then(c.certainty().total_cmp(&a.certainty())).then(a.id.cmp(&c.id)));
    b.sure = questions.iter().filter(|f| f.status == Status::SuggestSure).map(|f| f.id).collect();
    push_faces(&mut b, SectionKind::Questions, &questions);
    let mut doubtful: Vec<&library::face_index::Face> = ix.faces.iter().filter(|f| f.status == Status::Doubtful && filter.status_ok(f.status) && pass(f)).collect();
    doubtful.sort_by(|a, c| c.certainty().total_cmp(&a.certainty()).then(a.id.cmp(&c.id)));
    push_faces(&mut b, SectionKind::Doubtful, &doubtful);
    // groups as stacks, largest first
    if filter.status_ok(Status::UnnamedGroup) {
        let mut groups: HashMap<i64, (usize, Option<(f32, i64)>)> = HashMap::new();
        for f in ix.faces.iter().filter(|f| f.status == Status::UnnamedGroup && pass(f)) {
            let Some(g) = f.group else { continue };
            let e = groups.entry(g).or_default();
            e.0 += 1;
            if e.1.is_none_or(|(q, _)| f.quality > q) {
                e.1 = Some((f.quality, f.id));
            }
        }
        let mut v: Vec<(i64, usize, Option<i64>)> = groups.into_iter().map(|(g, (n, best))| (g, n, best.map(|x| x.1))).collect();
        v.sort_by(|a, c| c.1.cmp(&a.1).then(a.0.cmp(&c.0)));
        let start = b.tiles.len();
        b.tiles.extend(v.into_iter().map(|(g, n, face)| Tile { kind: Kind::Group(g), face, count: n }));
        if b.tiles.len() > start {
            b.sections.push(Section { kind: SectionKind::Groups, range: start..b.tiles.len() });
        }
    }
    for (status, kind) in [(Status::Unnamed, SectionKind::Unnamed), (Status::Rejected, SectionKind::Rejected)] {
        if filter.status_ok(status) {
            let mut v: Vec<&library::face_index::Face> = ix.faces.iter().filter(|f| f.status == status && pass(f)).collect();
            v.sort_by(|a, c| c.quality.total_cmp(&a.quality).then(a.id.cmp(&c.id)));
            push_faces(&mut b, kind, &v);
        }
    }
    b
}

fn push_faces(b: &mut Built, kind: SectionKind, faces: &[&library::face_index::Face]) {
    if faces.is_empty() {
        return;
    }
    let start = b.tiles.len();
    b.tiles.extend(faces.iter().map(|f| Tile { kind: Kind::Face(f.id), face: Some(f.id), count: 1 }));
    b.sections.push(Section { kind, range: start..b.tiles.len() });
}

/// People whose name contains the typed text (case and accents ignored), alphabetically.
pub fn matching_people<'a>(ix: &'a FaceIndex, text: &str) -> Vec<&'a library::face_index::Person> {
    let k = library::people::person_brief(text);
    if k.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&library::face_index::Person> = ix.people.iter().filter(|p| library::people::person_brief(&p.name).contains(&k)).collect();
    // names starting with the text first, otherwise alphabetical (the index is sorted already)
    v.sort_by_key(|p| !library::people::person_brief(&p.name).starts_with(&k));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use library::face_index::{Face, Person};

    fn face(id: i64, person: Option<i64>, status: Status) -> Face {
        Face { id, image: id, person, suspect: None, status, value: 0.0, second: 0.0, group: None, source: Source::Own, quality: 0.5, year: 2024, capture: format!("2024-01-{:02}", id % 28 + 1), file_name: String::new() }
    }

    fn index() -> FaceIndex {
        let mut faces = vec![face(1, Some(10), Status::Confirmed), face(2, Some(10), Status::Confirmed), face(3, Some(11), Status::Confirmed)];
        let mut s = face(4, Some(10), Status::SuggestSure);
        s.value = 0.8;
        faces.push(s);
        let mut m = face(5, Some(10), Status::SuggestMaybe);
        m.value = 0.62;
        faces.push(m);
        let mut d = face(6, Some(11), Status::Doubtful);
        d.suspect = Some(10);
        faces.push(d);
        for id in 7..10 {
            let mut g = face(id, None, Status::UnnamedGroup);
            g.group = Some(7);
            faces.push(g);
        }
        faces.push(face(10, None, Status::Unnamed));
        let people = vec![Person { id: 10, name: "Anna".into(), hidden: false }, Person { id: 11, name: "Ben".into(), hidden: false }];
        let by_id = faces.iter().enumerate().map(|(i, f)| (f.id, i)).collect();
        FaceIndex { faces, people, by_id }
    }

    /// A name chosen: its confirmed faces as one stack, below the suggestions for it and the doubts pointing to it.
    #[test]
    fn one_person_gives_stack_then_suggestions() {
        let ix = index();
        let f = Filter { people: BTreeSet::from([10]), ..Default::default() };
        let b = build(&ix, &f, None);
        let kinds: Vec<SectionKind> = b.sections.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![SectionKind::People, SectionKind::Questions, SectionKind::Doubtful]);
        assert_eq!(b.tiles[0], Tile { kind: Kind::Person(10), face: Some(1), count: 2 });
        // very likely first
        assert_eq!(b.tiles[1].kind, Kind::Face(4));
        assert_eq!(b.tiles[2].kind, Kind::Face(5));
        assert_eq!(b.tiles[3].kind, Kind::Face(6), "Ben's face that looks like Anna is a question for Anna");
        assert_eq!(b.sure, vec![4]);
        // person column: confirmed · open, alphabetically, "(ohne Namen)" last
        assert_eq!(b.people[0], Count { value: 10, n: 2, open: 3 });
        assert_eq!(b.people[1], Count { value: 11, n: 2, open: 0 });
        assert_eq!(b.people.last().unwrap().value, NO_PERSON);
    }

    /// Without a choice: people stacks, suggestions, doubts and groups – single unnamed faces only on request.
    #[test]
    fn standard_view_and_status_counts() {
        let ix = index();
        let b = build(&ix, &Filter::default(), None);
        let kinds: Vec<SectionKind> = b.sections.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, vec![SectionKind::People, SectionKind::Questions, SectionKind::Doubtful, SectionKind::Groups]);
        let groups = &b.tiles[b.sections[3].range.clone()];
        assert_eq!(groups, &[Tile { kind: Kind::Group(7), face: Some(7), count: 3 }]);
        let n = |s: Status| b.status.iter().find(|c| c.value == s as i64).unwrap().n;
        assert_eq!((n(Status::Confirmed), n(Status::SuggestSure), n(Status::Unnamed)), (3, 1, 1));
        let f = Filter { status: BTreeSet::from([Status::Unnamed]), ..Default::default() };
        let b = build(&ix, &f, None);
        assert_eq!(b.tiles, vec![Tile { kind: Kind::Face(10), face: Some(10), count: 1 }]);
    }

    /// An opened stack shows the faces of the person (newest first) or of the group.
    #[test]
    fn opened_stack() {
        let ix = index();
        let b = build(&ix, &Filter::default(), Some(Stack::Person(10)));
        assert_eq!(b.tiles.iter().map(|t| t.kind).collect::<Vec<_>>(), vec![Kind::Face(2), Kind::Face(1)]);
        let b = build(&ix, &Filter::default(), Some(Stack::Group(7)));
        assert_eq!(b.tiles.len(), 3);
    }

    #[test]
    fn search_is_alphabetical_and_ignores_accents() {
        let mut ix = index();
        ix.people = vec![Person { id: 1, name: "Hannah".into(), hidden: false }, Person { id: 2, name: "Anne".into(), hidden: false }, Person { id: 3, name: "Zoë Ann".into(), hidden: false }];
        ix.people.sort_by_key(|p| library::people::person_brief(&p.name));
        let names: Vec<&str> = matching_people(&ix, "ann").iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Anne", "Hannah", "Zoë Ann"]);
        assert_eq!(matching_people(&ix, "zoe").len(), 1);
    }
}
