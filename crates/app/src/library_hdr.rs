//! HDR merge from the library: which photos are offered, creating the merged photo (on top of a stack with its sources)
//! and taking it back with ⌘Z. The dialog and the job are in `hdr_ui.rs`.

use super::*;
use crate::hdr_ui::{Request, SourceRow};

/// Longest pause between two frames that still counts as one exposure bracket (a slow bracket on a tripod).
const BRACKET_MAX_GAP_S: i64 = 120;

/// Why the chosen photos do not look like one exposure bracket (asked before merging, never refused).
#[derive(Clone, Debug, PartialEq)]
pub enum Doubt {
    /// seconds between two frames, more than [`BRACKET_MAX_GAP_S`]
    Gap(i64),
    /// neighbouring frames show different pictures (image signatures / CLIP of the series detection)
    Unlike,
}

impl Doubt {
    pub fn text(&self) -> String {
        match self {
            Doubt::Gap(s) => crate::trf!("Zwischen zwei Aufnahmen liegen {n} Minuten.", n = (s + 30) / 60),
            Doubt::Unlike => crate::tr("Die Bildinhalte unterscheiden sich deutlich.").to_string(),
        }
    }
}

/// The doubt about `rows` (dark → bright or any order): none when they are one detected series/bracket (`grouped`);
/// else a pause of more than two minutes between two shots, or neighbouring frames that do not look alike
/// (`alike`: from [`library::series::bracket_alike`], `None` = unknown).
pub(crate) fn bracket_doubt(rows: &[SourceRow], grouped: bool, alike: Option<bool>) -> Option<Doubt> {
    if grouped {
        return None;
    }
    let mut times: Vec<i64> = rows.iter().filter_map(|r| r.capture).collect();
    times.sort_unstable();
    let gap = times.windows(2).map(|w| w[1] - w[0]).max().unwrap_or(0);
    if gap > BRACKET_MAX_GAP_S {
        return Some(Doubt::Gap(gap));
    }
    (alike == Some(false)).then_some(Doubt::Unlike)
}

/// What "HDR zusammenfügen …" (menu, ⇧⌘H) does with the photos at hand (see [`LibraryView::hdr_pick`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Pick {
    /// merge these photos
    Merge(Vec<SourceRow>),
    /// merge the selection although the photo shown or active (named) is not part of it: asked first, never a silent
    /// switch to another photo or bracket
    Selection(Vec<SourceRow>, String),
    /// the photo at hand is a merged one: it is opened in the tool "Zusammenfügen" (a merge is never merged again)
    Open(i64),
    /// the photo at hand is an SFD shot (X3I): it holds its own bracket, which Develop merges when it opens it
    Develop(i64),
    /// the selection holds merged photos besides ordinary ones (QA 1.5.0-test.5: "HDR + 3 RAWs" merged the three
    /// silently): merged photos are never sources, so this is asked first. `left`: the merged photos left out (image,
    /// name); `own`: the first of them whose sources include every photo of `rows` (then it is offered to open);
    /// `outside`: as in [`Pick::Selection`]
    LeavesOut { rows: Vec<SourceRow>, left: Vec<(i64, String)>, own: Option<i64>, outside: Option<String> },
}

impl LibraryView {
    /// The photos "HDR zusammenfügen …" takes (QA 1.5.0-test.5: with seven photos of one bracket selected and another
    /// photo active, the bracket of the active photo was merged unasked). A selection of more than one photo always
    /// counts – asked about when the photo at hand (`shown`: the photo of the single view, else the active one) is not
    /// part of it; otherwise that photo: a merged one opens, an ordinary one brings its bracket or stack.
    pub fn hdr_pick(&self, shown: Option<i64>) -> Pick {
        let at = shown.or(self.active).or_else(|| (self.selection.len() == 1).then(|| self.selection.iter().next().copied()).flatten());
        if self.selection.len() > 1 {
            let mut selected: Vec<i64> = self.selection.iter().copied().collect();
            selected.sort_unstable();
            let rows = self.hdr_candidates_of(&selected);
            let left: Vec<(i64, String)> = selected.iter().filter(|&&b| self.is_merged_image(b)).filter_map(|&b| self.index.get(&b).map(|&i| (b, library::merge::shown_name(&self.rows[i].name, self.rows[i].status as i64)))).collect();
            if !left.is_empty() {
                let files: Vec<i64> = rows.iter().map(|r| r.file).collect();
                let own = left.iter().map(|(b, _)| *b).find(|&b| !files.is_empty() && self.merge_sources(b).is_some_and(|src| files.iter().all(|f| src.contains(f))));
                let outside = at.filter(|a| !self.selection.contains(a)).map(|a| self.index.get(&a).map(|&i| library::merge::shown_name(&self.rows[i].name, self.rows[i].status as i64)).unwrap_or_default());
                return Pick::LeavesOut { rows, left, own, outside };
            }
            return match at.filter(|a| !self.selection.contains(a)) {
                Some(a) => Pick::Selection(rows, self.index.get(&a).map(|&i| library::merge::shown_name(&self.rows[i].name, self.rows[i].status as i64)).unwrap_or_default()),
                None => Pick::Merge(rows),
            };
        }
        match at {
            Some(a) if self.is_merged_image(a) => Pick::Open(a),
            Some(a) if self.index.get(&a).is_some_and(|&i| self.rows[i].format == "X3I") => Pick::Develop(a),
            Some(a) => Pick::Merge(self.hdr_candidates(Some(a))),
            None => Pick::Merge(Vec::new()),
        }
    }

    /// The source files of a merged photo (`None`: not a merged photo or no recipe).
    fn merge_sources(&self, image: i64) -> Option<Vec<i64>> {
        let file = self.index.get(&image).map(|&i| self.rows[i].file)?;
        library::merge::recipe(&self.db.as_ref()?.conn, file).map(|r| r.sources.iter().map(|s| s.file).collect())
    }

    /// The merged photo already made of exactly these frames (the newest, when there are several): its image and name.
    pub fn existing_merge(&self, rows: &[SourceRow]) -> Option<(i64, String)> {
        let files: Vec<i64> = rows.iter().map(|r| r.file).collect();
        let made = library::merge::made_from(&self.db.as_ref()?.conn, &files);
        made.iter().rev().find_map(|f| self.rows.iter().find(|z| z.file == *f)).map(|z| (z.image, library::merge::shown_name(&z.name, z.status as i64)))
    }

    /// Show an existing merged photo to work on (Develop follows with `Action::Develop`): active, its collapsed stack
    /// opened when it is not the photo on top, the view changed only when it cannot show it (like a new merge).
    pub fn show_merge(&mut self, image: i64, actions: &mut Vec<Action>) {
        if let Some(k) = self.series.of(image) {
            let s = &mut self.series.stacks[k];
            if s.collapsed && s.top() != image {
                s.collapsed = false;
                let id = s.id;
                self.store_collapsed(vec![id], false);
            }
        }
        self.refilter = true;
        self.show_new_merge(image, None, true, actions);
    }

    /// Do these photos look like one exposure bracket? `None` when they do (all in one series or stack, a detected
    /// bracket, or nothing speaks against it); otherwise why not (the app asks before merging).
    pub fn hdr_doubt(&self, rows: &[SourceRow]) -> Option<Doubt> {
        let stack = self.series.of(rows.first()?.image);
        let one_stack = stack.is_some() && rows.iter().all(|r| self.series.of(r.image) == stack);
        let bracket = crate::hdr_batch::brackets(rows.to_vec()).iter().any(|g| g.len() == rows.len());
        if one_stack || bracket {
            return None;
        }
        // neighbours in shooting order
        let mut by_time: Vec<&SourceRow> = rows.iter().collect();
        by_time.sort_by_key(|r| r.capture.unwrap_or(i64::MAX));
        let files: Vec<i64> = by_time.iter().map(|r| r.file).collect();
        let alike = self.db.as_ref().and_then(|d| library::series::bracket_alike(&d.conn, &files).ok().flatten());
        bracket_doubt(rows, false, alike)
    }
    /// Photos offered for "HDR zusammenfügen …": the selection, or for a single photo in a stack the whole stack
    /// (merged photos themselves are never sources). `image`: start from this photo instead of the active one.
    pub fn hdr_candidates(&self, image: Option<i64>) -> Vec<SourceRow> {
        let mut target = match image {
            Some(b) if !self.selection.contains(&b) || self.selection.len() < 2 => vec![b],
            _ => self.target(),
        };
        if target.len() == 1 {
            let one = target[0];
            // the exposure bracket the photo belongs to (same detection as "Alle Reihen zusammenfügen …": wide brackets
            // included), from the photos shot around it
            if let Some(rows) = self.bracket_around(one) {
                return rows;
            }
            if let Some(k) = self.series.of(target[0]) {
                target = self.series.stacks[k].images.clone();
            }
        }
        self.hdr_candidates_of(&target)
    }

    /// The bracket of [`crate::hdr_batch::brackets`] that holds `image`, searched among the photos shot within 30 s of it.
    fn bracket_around(&self, image: i64) -> Option<Vec<SourceRow>> {
        let z = self.index.get(&image).map(|&i| &self.rows[i])?;
        let at = z.capture.as_deref().and_then(library::pairs::seconds)?;
        let near: Vec<i64> = self
            .rows
            .iter()
            .filter(|r| r.folder == z.folder && r.capture.as_deref().and_then(library::pairs::seconds).is_some_and(|t| (t - at).abs() <= 30))
            .map(|r| r.image)
            .collect();
        crate::hdr_batch::brackets(self.hdr_candidates_of(&near)).into_iter().find(|g| g.iter().any(|r| r.image == image))
    }

    /// Photos of the bracket or stack of a merged photo (`image`) that are not among its frames (`files`): offered
    /// unticked in the tool "Zusammenfügen".
    pub fn hdr_more_candidates(&self, image: i64, files: &[i64]) -> Vec<SourceRow> {
        let mut out: Vec<SourceRow> = Vec::new();
        let mut take = |rows: Vec<SourceRow>| {
            for r in rows {
                if !files.contains(&r.file) && !out.iter().any(|o| o.file == r.file) {
                    out.push(r);
                }
            }
        };
        if let Some(k) = self.series.of(image) {
            take(self.hdr_candidates_of(&self.series.stacks[k].images.clone()));
        }
        if let Some(first) = files.first().and_then(|f| self.rows.iter().find(|r| r.file == *f)).map(|r| r.image) {
            if let Some(rows) = self.bracket_around(first) {
                take(rows);
            }
        }
        out.truncate(hdr::MAX_FRAMES);
        out.sort_by(|a, b| a.exif_ev().unwrap_or(0.0).total_cmp(&b.exif_ev().unwrap_or(0.0)));
        out
    }

    /// The photos of these library files as merge sources (in this order; photos not in the view are left out).
    pub fn hdr_rows_of_files(&self, files: &[i64]) -> Vec<SourceRow> {
        let images: Vec<i64> = files.iter().filter_map(|f| self.rows.iter().find(|r| r.file == *f).map(|r| r.image)).collect();
        self.hdr_candidates_of(&images)
    }

    fn hdr_candidates_of(&self, target: &[i64]) -> Vec<SourceRow> {
        target
            .iter()
            .filter_map(|b| self.index.get(b).map(|&i| &self.rows[i]))
            .filter(|z| z.folder != VIEW_FOLDER && z.copy.is_none() && z.status as i64 != library::merge::STATUS_VIRTUAL)
            .map(|z| SourceRow {
                file: z.file,
                image: z.image,
                path: self.path_of(z),
                name: z.name.clone(),
                format: z.format.clone(),
                camera: z.camera.clone(),
                time: z.time,
                iso: z.iso,
                aperture: z.aperture,
                focal: z.focal,
                capture: z.capture.as_deref().and_then(library::pairs::seconds),
                width: z.width,
                height: z.height,
            })
            .collect()
    }

    /// What the library needs of a merge request, taken from the view (stack of the reference, source files).
    fn prepare_merge(&self, req: Request) -> Prepared {
        let files: Vec<i64> = req.rows.iter().map(|r| r.file).collect();
        let images: Vec<i64> = req.rows.iter().map(|r| r.image).collect();
        let reference = req.settings.reference.unwrap_or(req.rows.len() / 2).min(req.rows.len().saturating_sub(1));
        // the stack of the reference (all its photos stay together), otherwise the sources
        let mut members: Vec<i64> = self.series.of(images[reference]).map(|k| self.series.stacks[k].images.clone()).unwrap_or_default();
        for b in &images {
            if !members.contains(b) {
                members.push(*b);
            }
        }
        let settings = hdr::Settings { reference: Some(reference), ..req.settings };
        let settings_json = serde_json::to_string(&settings).unwrap_or_default();
        // the merged photo joins the collection being looked at (it holds the sources), so it shows where it was made
        let collection = match &self.source {
            Source::Collection(id) => Some(*id),
            Source::Quick => self.quick_id,
            _ => None,
        };
        // an expanded series stays expanded as the stack of the merge (no tiles vanish from the grid) – its stored
        // state: a series opened by ⌥ only in memory stays collapsed (QA 5)
        let expanded = self.series.of(images[reference]).is_some_and(|k| !self.series.stored_collapsed(k));
        Prepared {
            files,
            reference,
            reference_image: images[reference],
            members,
            settings,
            settings_json,
            stack: req.stack,
            expanded,
            collection,
            keyword: req.keyword,
            paths: req.rows.iter().map(|r| r.path.clone()).collect(),
        }
    }

    /// Create the merged photo of `req` and start computing it: a virtual file next to the reference, on top of a
    /// stack with its sources (the stack of the reference grows, otherwise the sources become one). ⌘Z removes it.
    pub fn start_merge(&mut self, req: Request) {
        let p = self.prepare_merge(req);
        let (settings, paths) = (p.settings.clone(), p.paths.clone());
        let stacked = p.stacked();
        let origin = self.active;
        self.in_background(
            move |db| create_merge(db, &p),
            move |b, actions, r| match r {
                Ok((c, change, key, name)) => {
                    if let Some(o) = origin {
                        MERGE_ORIGIN.lock().unwrap_or_else(|e| e.into_inner()).insert(c.file, o);
                    }
                    b.manage.push(library_manage::Undo::Merge { label: crate::tr("HDR zusammenfügen").to_string(), file: c.file, key: key.clone(), change: change.clone() });
                    crate::series_job::changed();
                    let base_key = library::merge::keys_for(c.file, &[&serde_json::to_string(&settings.base()).unwrap_or_default()]).and_then(|mut k| k.pop()).unwrap_or_default();
                    let src = rawdev::merged::MergedSource::new(c.file, paths, settings, key, base_key);
                    crate::hdr_ui::forget_session(c.file);
                    crate::hdr_ui::push(crate::hdr_ui::Event::New { file: c.file });
                    crate::hdr_ui::run(src, name, change);
                    // straight to the new photo in Develop (the reference stands in until the merge is there,
                    // `hdr_live.rs`) – only once the reloaded rows hold it: before, Develop took a photo of the old
                    // list instead (test 1.5.0-test.4: an X3F of another series opened)
                    let image = c.image;
                    b.reload_then(move |b, actions| b.show_new_merge(image, stacked, true, actions));
                    let _ = actions;
                }
                Err(e) => actions.push(Action::Status(crate::trf!("HDR nicht angelegt: {e}", e = e))),
            },
        );
    }

    /// Create the merged photos of several brackets (one request each, sorted dark → bright) and compute them one after
    /// the other in one background job (`hdr_batch`); ⌘Z takes the whole batch back.
    pub fn start_merge_batch(&mut self, reqs: Vec<Request>) {
        // (counts as running from here on: a second batch confirmed before the library has written this one would
        // share its progress, its cancel flag and its ⌘Z step)
        if !crate::hdr_batch::reserve() {
            crate::hdr_live::say(crate::tr(crate::hdr_batch::RUNNING));
            return;
        }
        let prepared: Vec<Prepared> = reqs.into_iter().filter(|r| r.rows.len() >= hdr::MIN_FRAMES).map(|r| self.prepare_merge(r)).collect();
        if prepared.is_empty() {
            crate::hdr_batch::release();
            return;
        }
        let total = prepared.len();
        if self.db.is_none() {
            crate::hdr_batch::release();
            return;
        }
        self.in_background(
            move |db| {
                let mut made = Vec::new();
                let mut failed = 0usize;
                for p in &prepared {
                    match create_merge(db, p) {
                        Ok((c, change, key, name)) => {
                            let base_key = library::merge::recipe(&db.conn, c.file).map(|r| r.cache_key_for(&db.conn, &serde_json::to_string(&p.settings.base()).unwrap_or_default())).unwrap_or_default();
                            made.push((crate::hdr_batch::Item { src: rawdev::merged::MergedSource::new(c.file, p.paths.clone(), p.settings.clone(), key, base_key), name, undo: change }, c.image, p.stacked()))
                        }
                        Err(_) => failed += 1,
                    }
                }
                Ok((made, failed))
            },
            move |b, actions, r| match r {
                Ok((made, failed)) if !made.is_empty() => {
                    let items: Vec<(i64, String, Option<library::series::Change>)> = made.iter().map(|(i, _, _)| (i.src.file, i.src.key.clone(), i.undo.clone())).collect();
                    b.manage.push(library_manage::Undo::MergeBatch { label: crate::tr("Alle Reihen zusammenfügen").to_string(), items });
                    crate::series_job::changed();
                    for (i, _, _) in &made {
                        crate::hdr_ui::forget_session(i.src.file);
                        crate::hdr_ui::push(crate::hdr_ui::Event::New { file: i.src.file });
                    }
                    let stacks: Vec<(i64, Option<(Vec<i64>, bool)>)> = made.iter().map(|(_, image, s)| (*image, s.clone())).collect();
                    b.reload_then(move |b, actions| {
                        // every new stack known at once, the first merge shown
                        for (image, s) in stacks.iter().skip(1) {
                            if let Some((members, expanded)) = s {
                                b.adopt_merge_stack(*image, members, *expanded);
                            }
                        }
                        let (first, s) = stacks[0].clone();
                        b.show_new_merge(first, s, false, actions);
                    });
                    crate::hdr_batch::run(made.into_iter().map(|(i, _, _)| i).collect());
                    let text = if failed > 0 { crate::trf!("{n} Reihen werden berechnet, {f} nicht angelegt – ⌘Z macht alles rückgängig", n = total - failed, f = failed) } else { crate::trf!("{n} Reihen werden berechnet – ⌘Z macht alles rückgängig", n = total) };
                    actions.push(Action::Status(text));
                }
                Ok(_) => {
                    crate::hdr_batch::release();
                    actions.push(Action::Status(crate::tr("HDR nicht angelegt").to_string()))
                }
                Err(e) => {
                    crate::hdr_batch::release();
                    actions.push(Action::Status(crate::trf!("HDR nicht angelegt: {e}", e = e)))
                }
            },
        );
    }

    /// The exposure brackets of the selection for "Alle Reihen zusammenfügen …": every series or stack that holds a
    /// selected photo and differs in exposure (detected brackets, other stacks with at least one EV of spread).
    pub fn hdr_groups(&self) -> Vec<crate::hdr_batch::Group> {
        self.hdr_groups_of(self.target())
    }

    /// Test hook (`GRAINEO_DIALOG=hdr-all`): the brackets of every photo of the library.
    pub fn hdr_test_groups(&self) -> Vec<crate::hdr_batch::Group> {
        self.hdr_groups_of(self.rows.iter().map(|z| z.image).collect())
    }

    fn hdr_groups_of(&self, images: Vec<i64>) -> Vec<crate::hdr_batch::Group> {
        // the selected photos and everything in their stacks (a collapsed stack stands for all its photos)
        let mut all: Vec<i64> = Vec::new();
        for b in images {
            let members = self.series.of(b).map(|k| self.series.stacks[k].images.clone()).unwrap_or_else(|| vec![b]);
            for m in members {
                if !all.contains(&m) {
                    all.push(m);
                }
            }
        }
        let merged_in_stack = |image: i64| {
            self.series.of(image).is_some_and(|k| self.series.stacks[k].images.iter().any(|b| self.index.get(b).is_some_and(|&i| self.rows[i].status as i64 == library::merge::STATUS_VIRTUAL)))
        };
        let mut groups: Vec<crate::hdr_batch::Group> = crate::hdr_batch::brackets(self.hdr_candidates_of(&all))
            .into_iter()
            .filter_map(|rows| {
                let merged = merged_in_stack(rows[rows.len() / 2].image);
                crate::hdr_batch::Group::new(rows, merged)
            })
            .collect();
        groups.sort_by(|a, b| a.time().total_cmp(&b.time()));
        groups
    }

    /// Test hook (`GRAINEO_DIALOG=hdr`): the first nine ordinary photos of the library.
    pub fn hdr_test_candidates(&self) -> Vec<SourceRow> {
        let images: Vec<i64> = self.rows.iter().filter(|z| z.status as i64 != library::merge::STATUS_VIRTUAL && z.copy.is_none()).take(hdr::MAX_FRAMES).map(|z| z.image).collect();
        images.iter().flat_map(|&b| self.hdr_candidates_of(&[b])).collect()
    }

    /// Is this photo a merged one (HDR)?
    pub fn is_merged_image(&self, image: i64) -> bool {
        self.index.get(&image).is_some_and(|&i| self.rows[i].status as i64 == library::merge::STATUS_VIRTUAL)
    }

    /// "Quellen zeigen": the stack of the merged photo opens in the grid, its sources selected.
    pub fn show_sources(&mut self, image: i64, actions: &mut Vec<Action>) {
        let Some(k) = self.series.of(image) else {
            actions.push(Action::Status(crate::tr("Das HDR liegt in keinem Stapel.").to_string()));
            return;
        };
        let s = &mut self.series.stacks[k];
        s.collapsed = false;
        let (id, members) = (s.id, s.images.clone());
        self.selection = members.iter().copied().filter(|b| *b != image).collect();
        self.nothing_selected = false;
        self.refilter = true;
        self.store_collapsed(vec![id], false);
        let _ = actions;
        crate::hdr_live::show_grid();
    }

    /// "HDR entfernen (Quellen bleiben)": the merged photo leaves the library (row, recipe, edits); the sources are never
    /// touched. One ⌘Z step brings it back as it was. A photo that carries work of its own (painted areas, a
    /// development by hand, virtual copies) is asked about first.
    pub fn remove_merge(&mut self, file: i64, actions: &mut Vec<Action>) {
        let _ = actions;
        let lines = self.merge_work_lines(file);
        if lines.is_empty() {
            self.remove_merge_now(vec![file]);
            return;
        }
        let name = self.rows.iter().find(|r| r.file == file).map(|z| library::merge::shown_name(&z.name, z.status as i64)).unwrap_or_default();
        self.manage.ask = Some(library_manage::Ask::RemoveMerge { files: vec![file], name, lines, undo: false });
    }

    /// What the merged photo `file` carries beyond the merge itself, as dialog lines (empty: nothing, it can be made
    /// again as it is). Values the automatic tone set and nobody moved do not count as a development.
    pub(super) fn merge_work_lines(&self, file: i64) -> Vec<String> {
        use crate::dialog::count;
        let Some(db) = self.db.as_ref() else { return Vec::new() };
        let w = library::merge::work(&db.conn, file);
        let auto = library::merge::recipe(&db.conn, file).and_then(|r| rawdev::merged::parse_settings(&r.settings).ok()).and_then(|s| s.auto_tone).and_then(|a| a.applied);
        let developed = w.develop.iter().filter_map(|j| serde_json::from_str::<crate::Settings>(j).ok()).any(|s| crate::hdr_auto::edited_by_hand(&s, auto));
        let mut v = Vec::new();
        if w.areas > 0 {
            v.push(count(w.areas, crate::tr("gemalter Bereich"), crate::tr("gemalte Bereiche")));
        }
        if developed {
            v.push(crate::tr("eigene Entwicklung").to_string());
        }
        if w.copies > 0 {
            v.push(count(w.copies, crate::tr("virtuelle Kopie"), crate::tr("virtuelle Kopien")));
        }
        v
    }

    /// The library file at `path` (as the view knows it).
    pub fn file_at(&self, path: &Path) -> Option<i64> {
        let want = library::nfc(&path.to_string_lossy());
        self.rows.iter().find(|z| z.copy.is_none() && library::nfc(&self.path_of(z).to_string_lossy()) == want).map(|z| z.file)
    }

    /// The path the merged photo `file` is addressed by (read from the library).
    pub fn merged_path(&self, file: i64) -> Option<PathBuf> {
        library::merge::file_path(&self.db.as_ref()?.conn, file)
    }

    /// Does the library hold the merged photo `file` (as the view knows it)?
    pub fn has_merged_file(&self, file: i64) -> bool {
        self.rows.iter().any(|z| z.file == file && z.status as i64 == library::merge::STATUS_VIRTUAL)
    }

    /// Take merged photos out of the library as one ⌘Z step (their cached results stay until the step expires).
    ///
    /// Carried out by [`Self::run_merge_removals`] (the app calls it in the same frame, after it handed the develop
    /// settings of the open photo to the library: a slider moved just before would otherwise be written after the
    /// photo left, dropped, and missing when ⌘Z brings the photo back).
    pub(super) fn remove_merge_now(&mut self, files: Vec<i64>) {
        self.manage.merge_removals.push(files);
    }

    /// Are merged photos waiting to be taken out ([`Self::remove_merge_now`])?
    pub fn merge_removals_waiting(&self) -> bool {
        !self.manage.merge_removals.is_empty()
    }

    /// Take out the merged photos asked for.
    pub fn run_merge_removals(&mut self) {
        for files in std::mem::take(&mut self.manage.merge_removals) {
            self.take_merges_out(files);
        }
    }

    fn take_merges_out(&mut self, files: Vec<i64>) {
        for f in &files {
            // (its jobs stop and leave the photo alone: it is taken out here, with everything ⌘Z needs to bring it back)
            crate::hdr_ui::cancel_and_keep(*f);
        }
        // a ⌘Z of creating it that still waited for its first computation has nothing left to do: the photo goes
        // here (carried out later it would take the photo out once more after ⌘Z brought it back)
        self.manage.merge_undo_waiting.retain(|f| !files.contains(f));
        let taken = files.clone();
        self.in_background(
            move |db| {
                // develop settings of the photo still on their way into the library are part of what ⌘Z brings back
                // (written after the photo left they were dropped: the development was lost)
                library::store::flush(std::time::Duration::from_secs(5));
                library::merge::take_out(db, &files)
            },
            move |b, actions, r| match r {
                Ok(snap) => {
                    b.manage.push(library_manage::Undo::MergeRemoved { label: crate::tr("HDR entfernen").to_string(), snap, files: taken });
                    crate::series_job::changed();
                    b.reload();
                    actions.push(Action::Status(crate::tr("HDR entfernt, die Quellen bleiben – ⌘Z holt es zurück").to_string()));
                }
                Err(e) => actions.push(Action::Status(crate::trf!("HDR nicht entfernt: {e}", e = e))),
            },
        );
    }

    /// "Rückgängig" of the hint after a merge: the step of creating it when it is the last one of the library (the stack
    /// goes back as it was), otherwise the photo is removed (the sources stay either way).
    pub fn undo_merge(&mut self, file: i64) -> Vec<Action> {
        let mut actions = Vec::new();
        if matches!(self.manage.undo.last(), Some(library_manage::Undo::Merge { file: f, .. }) if *f == file) {
            self.manage_undo();
        } else {
            self.remove_merge(file, &mut actions);
        }
        actions
    }

    pub fn rows_loaded(&self) -> bool {
        !self.rows.is_empty()
    }

    /// Merged photos whose grid preview is gone (cache cleared): their sources and recipe, to render it once from the
    /// cached merge preview (never a new merge).
    pub fn hdr_missing_previews(&self) -> Vec<(i64, rawdev::merged::MergedSource)> {
        self.rows
            .iter()
            .filter(|z| z.status as i64 == library::merge::STATUS_VIRTUAL && !library::preview::has_any_developed(z.file))
            .filter_map(|z| rawdev::merged::lookup(&self.path_of(z)).map(|src| (z.file, src)))
            .collect()
    }

    /// A merged photo just created (rows reloaded): make it the active photo of the view and, with `develop`, open it.
    /// It never leaves the view: when the source or a filter hides it, the folder of its reference is shown instead,
    /// then also without filters – with a short note. Never another photo.
    /// `stacked`: the members of its new stack and whether that is expanded (`None`: a photo of its own).
    pub(super) fn show_new_merge(&mut self, image: i64, stacked: Option<(Vec<i64>, bool)>, develop: bool, actions: &mut Vec<Action>) {
        if let Some((members, expanded)) = &stacked {
            self.adopt_merge_stack(image, members, *expanded);
        }
        let Some(&i) = self.index.get(&image) else {
            actions.push(Action::Status(crate::tr("HDR nicht gefunden – die Bibliothek wird neu gelesen.").to_string()));
            return;
        };
        let folder = self.rows[i].folder;
        let shown = |b: &mut LibraryView| {
            b.apply_filter();
            b.visible.iter().any(|&r| b.rows[r].image == image)
        };
        if !shown(self) {
            let name = self.folder.iter().find(|o| o.id == folder).map(|o| o.path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()).unwrap_or_default();
            self.show_folder(image);
            let note = if shown(self) {
                crate::trf!("Das HDR liegt nicht in dieser Ansicht – Ordner „{a}“ gezeigt", a = name)
            } else {
                self.reset_all_filters();
                self.show_folder(image);
                shown(self);
                crate::trf!("Das HDR liegt nicht in dieser Ansicht – Ordner „{a}“ ohne Filter gezeigt", a = name)
            };
            actions.push(Action::Status(note));
        }
        // the sources selected for the merge are not the photo to work on any more (the filter may already have made
        // the HDR active as the tile of their collapsed stack, which keeps the selection)
        self.set_active(image);
        self.selection.clear();
        self.sel_ext = None;
        self.to_active = true;
        if develop {
            actions.push(Action::Develop);
        }
    }

    /// The stack a merge just created, known before the series are read again (a frame or more later): the merged
    /// photo on top of `members`, which leave their stacks – as `create_merge` wrote it. Without it the grid shows the
    /// HDR alone for a moment, and "Serien" does not show it at all.
    pub(super) fn adopt_merge_stack(&mut self, image: i64, members: &[i64], expanded: bool) {
        // a stack holding every source keeps its kind and takes the merged photo on top (as `create_merge` wrote it)
        if let Some(k) = members.first().and_then(|m| self.series.of(*m)).filter(|&k| members.iter().all(|m| self.series.stacks[k].images.contains(m))) {
            if let Some(old) = self.series.of(image).filter(|o| *o != k) {
                self.series.stacks[old].images.retain(|i| *i != image);
            }
            let s = &mut self.series.stacks[k];
            s.images.retain(|i| *i != image);
            s.images.insert(0, image);
            s.cover = Some(image);
            s.cover_by_user = true;
            s.origin = "user".into();
            s.collapsed = !expanded;
            self.series.stacks.retain(|s| s.images.len() >= 2);
            self.series.by_image.clear();
            for (k, s) in self.series.stacks.iter().enumerate() {
                for &i in &s.images {
                    self.series.by_image.insert(i, k);
                }
            }
            self.refilter = true;
            return;
        }
        let mut images = vec![image];
        images.extend(members.iter().copied().filter(|m| *m != image));
        for s in &mut self.series.stacks {
            s.images.retain(|i| !images.contains(i));
        }
        self.series.stacks.retain(|s| !s.images.is_empty());
        self.series.stacks.push(library::series::Stack {
            id: 0,
            kind: library::series::Kind::Manual,
            origin: "user".into(),
            cover: Some(image),
            cover_by_user: true,
            collapsed: !expanded,
            reviewed: None,
            reason: Default::default(),
            day: None,
            images,
        });
        self.series.by_image.clear();
        for (k, s) in self.series.stacks.iter().enumerate() {
            for &i in &s.images {
                self.series.by_image.insert(i, k);
            }
        }
        self.refilter = true;
    }

    /// Re-read the library after a merge job removed its photo again (cancelled, failed).
    pub fn merge_removed(&mut self) {
        crate::series_job::changed();
        self.reload();
    }

    /// A new merged photo `file` left the library again (its first computation was cancelled or failed): the photo
    /// it was made from becomes the active one again – not whichever neighbour took the place (QA 11, N-1).
    pub fn merge_removed_back(&mut self, file: i64) {
        let origin = MERGE_ORIGIN.lock().unwrap_or_else(|e| e.into_inner()).remove(&file);
        crate::series_job::changed();
        match origin {
            Some(image) => self.reload_then(move |b, _| {
                if b.index.contains_key(&image) {
                    b.set_active(image);
                }
            }),
            None => self.reload(),
        }
    }
}

/// The photo each new merged photo was made from (active when it was created): shown again when the merge is taken
/// out at once ([`LibraryView::merge_removed_back`]).
static MERGE_ORIGIN: std::sync::Mutex<std::collections::BTreeMap<i64, i64>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

impl LibraryView {
    /// Changed settings of a merged photo (reference, motion, output, alignment, areas): `save` is stored in its recipe
    /// (`None`: only a comparison, e.g. an area hidden), `view` is computed in the background – the newest change
    /// cancels older ones, the open photo takes the pixels in place (`hdr_live.rs`).
    pub fn update_merge(&mut self, file: i64, view: hdr::Settings, save: Option<hdr::Settings>, name: String) {
        // the order of the changes is the order the user made them in, whatever the background does with them
        let ticket = crate::hdr_ui::ticket();
        crate::hdr_live::asked(file);
        let start = move || {
            crate::hdr_ui::spawn(move || {
                if let Some(src) = rawdev::merged::source_for(file, &view) {
                    crate::hdr_live::note_change(&src.key);
                    crate::hdr_ui::run_update_as(src, name, ticket);
                }
            });
        };
        let Some(save) = save else {
            start();
            return;
        };
        let json = serde_json::to_string(&save).unwrap_or_default();
        self.in_background(
            move |db| rawdev::merged::check_recipe(db, file).and_then(|()| library::merge::set_settings(db, file, &json)),
            move |_b, actions, r| match r {
                Ok(_) => {
                    crate::hdr_paint::recipe_changed();
                    start()
                }
                Err(e) => {
                    crate::hdr_paint::change_arrived();
                    actions.push(Action::Status(crate::trf!("HDR nicht geändert: {e}", e = e)))
                }
            },
        );
    }

    /// Store the recipe settings of a merged photo without computing anything (the automatic tone: it changes no
    /// pixels, the cache key stays).
    pub fn store_merge_auto(&mut self, file: i64, auto: Option<hdr::AutoTone>) {
        self.in_background(
            // only the automatic tone, into the recipe as it is stored then: the settings the caller saw may be older
            // than a change of the recipe that is stored in between (it would be overwritten)
            move |db| {
                let r = library::merge::recipe(&db.conn, file).ok_or_else(|| format!("{}: sources", rawdev::merged::UNREADABLE))?;
                let mut settings = rawdev::merged::parse_settings(&r.settings)?;
                settings.auto_tone = auto;
                library::merge::set_settings(db, file, &serde_json::to_string(&settings).map_err(|e| e.to_string())?)
            },
            move |_b, actions, r| {
                crate::hdr_paint::recipe_changed();
                if let Err(e) = r {
                    actions.push(Action::Status(crate::trf!("HDR nicht geändert: {e}", e = e)));
                }
            },
        );
    }

    /// A frame taken in or left out: sources and settings of the recipe together (indices of the new list), then as
    /// [`Self::update_merge`].
    pub fn update_merge_sources(&mut self, file: i64, sources: Vec<i64>, settings: hdr::Settings, name: String) {
        let json = serde_json::to_string(&settings).unwrap_or_default();
        let ticket = crate::hdr_ui::ticket();
        crate::hdr_live::asked(file);
        self.in_background(
            move |db| rawdev::merged::check_recipe(db, file).and_then(|()| library::merge::set_sources(db, file, &sources, &json)),
            move |_b, actions, r| match r {
                Ok(_) => {
                    crate::hdr_paint::recipe_changed();
                    crate::hdr_ui::spawn(move || {
                        if let Some(src) = rawdev::merged::source_for(file, &settings) {
                            crate::hdr_live::note_change(&src.key);
                            crate::hdr_ui::run_update_as(src, name, ticket);
                        }
                    });
                }
                Err(e) => {
                    crate::hdr_paint::change_arrived();
                    actions.push(Action::Status(crate::trf!("HDR nicht geändert: {e}", e = e)))
                }
            },
        );
    }
}

/// Does the merged photo `file` carry work of the user beyond the merge itself: painted areas, virtual copies, or a
/// development by hand (values the automatic tone set and nobody moved do not count)?
pub(crate) fn has_own_work(db: &Db, file: i64) -> bool {
    let c = &db.conn;
    let w = library::merge::work(c, file);
    let auto = library::merge::recipe(c, file).and_then(|r| rawdev::merged::parse_settings(&r.settings).ok()).and_then(|s| s.auto_tone).and_then(|a| a.applied);
    w.areas > 0 || w.copies > 0 || w.develop.iter().filter_map(|j| serde_json::from_str::<crate::Settings>(j).ok()).any(|s| crate::hdr_auto::edited_by_hand(&s, auto))
}

/// A merge request as the library creates it (see [`LibraryView::prepare_merge`]).
struct Prepared {
    files: Vec<i64>,
    reference: usize,
    reference_image: i64,
    members: Vec<i64>,
    settings: hdr::Settings,
    settings_json: String,
    stack: bool,
    /// the stack of the merge starts expanded (the reference's series was)
    expanded: bool,
    /// collection (or quick collection) the merged photo joins
    collection: Option<i64>,
    keyword: String,
    paths: Vec<PathBuf>,
}

impl Prepared {
    /// The stack the merge goes into: its other members and whether it starts expanded (`None`: no stack).
    fn stacked(&self) -> Option<(Vec<i64>, bool)> {
        self.stack.then(|| (self.members.clone(), self.expanded))
    }
}

/// Library side of a merge: the virtual file, the stack, the collection, the keywords, the cache key – in one
/// transaction: the merged photo is either there with all of it (and the caller gets what it needs for ⌘Z and for the
/// job) or not at all. Never a photo in the library that was reported as "not created".
fn create_merge(db: &library::db::Db, p: &Prepared) -> Result<(library::merge::Created, Option<library::series::Change>, String, String), String> {
    let tx = db.begin_write()?;
    let c = library::merge::create_in(&tx, "hdr", &p.files, p.reference, &p.settings_json, hdr::ALGO_VERSION)?;
    let mut all = vec![c.image];
    all.extend(p.members.iter().copied());
    // in the stack of the reference (default) or as a photo of its own beside the sources. A stack that holds every
    // source takes the merged photo on top and stays what it is (a bracket stays a bracket, QA 1.5.0-test.5: it
    // became a stack of its own); sources from elsewhere make one new stack of all of them.
    let holding = library::series::stack_of(&tx, p.reference_image)?
        .and_then(|id| library::series::get(&tx, id).ok().flatten())
        .filter(|s| p.members.iter().all(|m| s.images.contains(m)));
    let change = if p.stack && holding.is_some() {
        let stack = holding.map(|s| s.id).unwrap_or_default();
        let change = library::series::join_in(&tx, stack, c.image)?;
        if p.expanded {
            library::series::set_collapsed(db, stack, false)?;
        }
        Some(change)
    } else if p.stack {
        let (change, stack) = library::series::create_in(&tx, &all, c.image)?;
        if p.expanded {
            library::series::set_collapsed(db, stack, false)?;
        }
        Some(change)
    } else {
        None
    };
    if let Some(id) = p.collection {
        library::db::Db::add_to_collection_in(&tx, id, &[c.image]).map_err(|e| e.to_string())?;
    }
    // the photo starts with the keywords of its reference, plus the keyword of the result (filterable later)
    let mut words = library::sync::keyword_paths(db, p.reference_image)?;
    let word = p.keyword.trim().to_string();
    if !word.is_empty() && !words.contains(&word) {
        words.push(word);
    }
    if !words.is_empty() {
        // (a savepoint inside this transaction)
        library::sync::set_keywords(db, c.image, &words, library::now(), true)?;
    }
    // unfinished until its first result is there: found at the next start when the app ends before
    library::merge::note_unfinished(&tx, c.file, change.as_ref())?;
    let recipe = library::merge::recipe(&tx, c.file).ok_or_else(|| crate::tr("Rezept fehlt").to_string())?;
    let key = recipe.cache_key(&tx);
    let name: String = tx.query_row("SELECT name FROM file WHERE id = ?1", [c.file], |r| r.get(0)).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    UNFINISHED.lock().unwrap_or_else(|e| e.into_inner()).push(c.file);
    Ok((c, change, key, library::merge::shown_name(&name, library::merge::STATUS_VIRTUAL)))
}

/// Merged photos created in this session whose first result is not there yet (the library's note of them is taken
/// back when it is, [`finished`]).
static UNFINISHED: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// A result of the merged photo `file` was computed: it is not unfinished any more (job thread).
pub(crate) fn finished(file: i64) {
    let noted = {
        let mut u = UNFINISHED.lock().unwrap_or_else(|e| e.into_inner());
        let n = u.len();
        u.retain(|f| *f != file);
        u.len() != n
    };
    if noted {
        if let Ok(db) = library::db::Db::standard() {
            let _ = library::merge::note_finished(&db.conn, &[file]);
        }
    }
}

/// Test: a new session (nothing created in it yet).
#[cfg(test)]
pub(crate) fn test_new_session() {
    UNFINISHED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// What became of the merged photos an earlier session left unfinished ([`settle_unfinished`]).
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Settled {
    /// removed: nothing was ever computed for them and they carried no work
    pub removed: Vec<i64>,
    /// kept: they show a picture or carry work (computed when they are opened)
    pub kept: Vec<i64>,
}

/// Is an unfinished merged photo removed at the start? Only when nothing of it exists but its row: no result in the
/// cache, no grid preview, no work of the user. Everything else stays (and is computed when it is opened).
pub(crate) fn unfinished_goes(has_result: bool, has_preview: bool, has_work: bool) -> bool {
    !has_result && !has_preview && !has_work
}

/// The merged photos an earlier session left unfinished – the app ended while they were computed; a batch creates all
/// its photos before it computes the first. Those nothing was ever computed for and nobody worked on leave the
/// library again, their stacks go back as they were (as when the merge is cancelled): they were empty tiles on top of
/// their stacks, without a ⌘Z step. The others stay. Photos a job of this session works on are left alone.
pub(crate) fn settle_unfinished(db: &Db) -> Result<Settled, String> {
    let list = library::merge::unfinished(&db.conn);
    let mut out = Settled::default();
    let mut done = Vec::new();
    for u in list {
        let file = u.file;
        if UNFINISHED.lock().unwrap_or_else(|e| e.into_inner()).contains(&file) || crate::hdr_ui::job_alive(file) || crate::hdr_batch::pending(file) {
            continue;
        }
        done.push(file);
        if !library::merge::is_virtual(&db.conn, file) {
            continue;
        }
        if !unfinished_goes(rawdev::merged::has_cached(file), library::preview::has_any_developed(file), has_own_work(db, file)) {
            out.kept.push(file);
            continue;
        }
        // (the stack back and the photo out in one transaction; a stack changed since is left as it is)
        match library::merge::undo_create(db, &[(file, u.change.clone())]).or_else(|_| library::merge::undo_create(db, &[(file, None)])) {
            Ok(_) => {
                crate::hdr_ui::forget_file(file);
                out.removed.push(file);
            }
            Err(e) => {
                crate::log(&format!("hdr: unfinished merge {file} not removed: {e}"));
                out.kept.push(file);
            }
        }
    }
    library::merge::note_finished(&db.conn, &done)?;
    Ok(out)
}

impl LibraryView {
    /// Once per start: settle what an earlier session left unfinished ([`settle_unfinished`]), with a short note.
    pub fn settle_unfinished_merges(&mut self) {
        self.in_background(
            |db| settle_unfinished(db),
            |b, actions, r| {
                if let Ok(s) = r {
                    if !s.removed.is_empty() {
                        crate::series_job::changed();
                        b.reload();
                        let n = s.removed.len();
                        actions.push(Action::Status(if n == 1 { crate::tr("Ein unfertiges HDR vom letzten Mal entfernt – die Quellen sind unverändert").to_string() } else { crate::trf!("{n} unfertige HDR vom letzten Mal entfernt – die Quellen sind unverändert", n = n) }));
                    }
                }
            },
        );
    }
}

#[cfg(test)]
#[path = "library_hdr_test.rs"]
mod tests;

/// Random sequences of actions on a merged photo, checked after every step (`hdr_stress_test.rs`).
#[cfg(test)]
#[path = "hdr_stress_test.rs"]
mod stress;
