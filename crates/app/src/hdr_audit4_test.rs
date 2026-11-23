//! Fourth audit of "HDR zusammenfügen" (before 1.5.0): one test per finding, stating the behaviour that is expected.
//! The findings that are fixed run with the suite; the ones still open are ignored (run them with `-- --ignored`).

use super::*;

/// F1: deleting the photos of a bracket warns that their merged photo "stays with its finished picture" and only
/// cannot be changed or computed again. But a merged photo whose reference frame is gone cannot be opened or
/// exported at all, although its full result is in the cache: the camera data is read from the reference file.
#[test]
fn a_cached_result_survives_the_loss_of_its_reference() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Photo(0), Wait(7)], &|lab| {
        let path = lab.hdr_path.clone().ok_or("no merged photo")?;
        let (recipe, key) = lab.recipe().ok_or("no recipe")?;
        while rawdev::merged::cache_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if !rawdev::merged::cached(&key) {
            return Err("the full result is not in the cache".into());
        }
        let settings = rawdev::merged::parse_settings(&recipe.settings)?;
        let reference = settings.reference.ok_or("no reference")?;
        let k = lab.frames.iter().position(|f| f.0 == recipe.sources[reference].file).ok_or("the reference is not a frame")?;
        // a frame that is not the reference: the cached result still opens
        let other = lab.frames.iter().position(|f| recipe.sources.iter().any(|s| s.file == f.0) && f.0 != recipe.sources[reference].file).ok_or("no other frame")?;
        lab.act(SourceGone(other));
        match rawdev::RawDoc::open_for_output(&path, &crate::Settings::default(), &|| true) {
            Ok(Some(d)) if !d.stands_in() => {}
            Ok(_) => return Err("without a frame that is not the reference: the reference stands in".into()),
            Err(e) => return Err(format!("without a frame that is not the reference: {e}")),
        }
        lab.act(SourceBack);
        // the reference itself
        lab.act(SourceGone(k));
        if !rawdev::merged::cached(&key) {
            return Err("the full result left the cache".into());
        }
        let full = |when: &str| -> Check {
            match rawdev::RawDoc::open_for_output(&path, &crate::Settings::default(), &|| true) {
                Ok(Some(d)) if !d.stands_in() => d.render_full(&crate::Settings::default()).map(|_| ()).map_err(|e| format!("{when}: the result is cached, its full resolution fails: {e}")),
                Ok(_) => Err(format!("{when}: the output of the merged photo is not its result")),
                Err(e) => Err(format!("{when}: the result is cached, the output fails: {e}")),
            }
        };
        full("reference file gone")?;
        // the photo deleted for good: its library row goes as well (the recipe keeps the path it had)
        lab.db().remove_images(&[lab.frames[k].1], &[]).map_err(|e| e.to_string())?;
        full("reference deleted from the library")
    });
}

/// F4: a real file that appears in the folder under the name the merged photo is addressed by (`<stem>-HDR.dng`, the
/// name other programs give their HDR DNGs as well) never comes into the library: the sync finds the name taken and
/// says nothing.
#[test]
#[ignore = "audit4 F4: a file on disk with the name of a merged photo is silently left out of the library"]
fn a_file_with_the_name_of_a_merged_photo_comes_into_the_library() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70)], &|lab| {
        let path = lab.hdr_path.clone().ok_or("no merged photo")?;
        tiny_dng(&path, 1.0, 0.01, 50_000, 4711);
        let db = lab.db();
        let _ = library::reconcile::reconcile(&db, None, &|_| {});
        let dir = format!("{}/", lab.dir.display());
        let rows: i64 = db
            .conn
            .query_row("SELECT count(*) FROM file d JOIN folder o ON o.id = d.folder JOIN root_folder s ON s.id = o.root WHERE s.path = ?1 AND d.status <> 3", [dir], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(&path);
        // the seven frames and the new file
        if rows != 8 {
            return Err(format!("the file on disk is not in the library ({rows} ordinary photos in the folder, 8 files on disk)"));
        }
        Ok(())
    });
}

/// F5: the brackets of "Alle Reihen zusammenfügen …" are listed (and computed) in the order of the exposure time of
/// their darkest frame instead of the order they were shot in.
#[test]
fn brackets_of_a_batch_are_listed_in_shooting_order() {
    let row = |name: &str, capture: i64, t: f64| crate::hdr_ui::SourceRow {
        file: 1,
        image: 1,
        path: PathBuf::from(format!("/not/there/{name}")),
        name: name.into(),
        format: "CR2".into(),
        camera: Some("Canon".into()),
        time: Some(t),
        iso: Some(100),
        aperture: Some(8.0),
        focal: None,
        capture: Some(capture),
        width: Some(100),
        height: Some(80),
    };
    // the first bracket in daylight (short times), the second one at dusk an hour later (long times)
    let early = crate::hdr_batch::Group::new(vec![row("A1", 1000, 0.01), row("A2", 1001, 0.04), row("A3", 1002, 0.16)], false).unwrap();
    let late = crate::hdr_batch::Group::new(vec![row("B1", 4600, 0.001), row("B2", 4601, 0.004), row("B3", 4602, 0.016)], false).unwrap();
    let mut groups = vec![late, early];
    groups.sort_by(|a, b| a.time().total_cmp(&b.time()));
    assert_eq!(groups[0].rows[0].name, "A1", "the bracket shot first comes first");
}

/// F2: "Sicherung ansehen" starts the app on a copy of an older library ([`library::db_path`] is that copy) – with the
/// same merge cache. Thirty seconds after its start the cache is tidied against the library at hand
/// (`rawdev::merged::sweep`): the results of every merged photo the backup does not know (made after it) are removed,
/// also those that cannot be merged again because a source is not reachable. The test puts an older copy of the
/// library in place the way the view process has it and runs the same tidying.
#[test]
fn looking_at_an_older_backup_keeps_the_results_of_newer_merges() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Photo(0), Wait(7)], &|lab| {
        let (recipe, key) = lab.recipe().ok_or("no recipe")?;
        while rawdev::merged::cache_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if !rawdev::merged::cached(&key) {
            return Err("the full result is not in the cache".into());
        }
        // the library as a backup made before the merge has it
        let copy = lab.dir.join("older-library.sqlite");
        lab.db().conn.execute("VACUUM INTO ?1", [copy.to_string_lossy().to_string()]).map_err(|e| e.to_string())?;
        {
            let older = Db::open(&copy).map_err(|e| e.to_string())?;
            older.conn.execute_batch("DELETE FROM merge; DELETE FROM file WHERE status = 3;").map_err(|e| e.to_string())?;
        }
        // a frame that is not the reference is not reachable: the result cannot be merged again
        let settings = rawdev::merged::parse_settings(&recipe.settings)?;
        let reference = recipe.sources[settings.reference.ok_or("no reference")?].file;
        let other = lab.frames.iter().position(|f| recipe.sources.iter().any(|s| s.file == f.0) && f.0 != reference).ok_or("no other frame")?;
        lab.act(SourceGone(other));
        // (the process that looks at a backup: its library is the copy, and it knows that it is not the real one)
        rawdev::merged::test_other_library(true);
        std::env::set_var("GRAINEO_LIBRARY_DB", &copy);
        let freed = rawdev::merged::sweep() + rawdev::merged::clear_cache() + rawdev::merged::tidy(lab.hdr.unwrap_or(0), true);
        crate::hdr_ui::forget_file(lab.hdr.unwrap_or(0));
        std::env::remove_var("GRAINEO_LIBRARY_DB");
        rawdev::merged::test_other_library(false);
        if !rawdev::merged::cached(&key) {
            return Err(format!("the result of the merged photo left the cache ({freed} bytes removed) although its library still has the photo and a source is not reachable"));
        }
        Ok(())
    });
}

/// F0: at full resolution an area ("Geister entfernen") takes its pixels from the RAW of its frame. When that file
/// cannot be read while the result is computed and the base of the merge is in the cache, the area was silently left
/// out of the full resolution and the result stored as complete. Now: the tool takes no change while a source is
/// gone, a computation that needs the file fails with its name and stores nothing, and with the source back the
/// result is the merge of its recipe.
#[test]
fn an_area_is_never_left_out_of_the_full_resolution_silently() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobsFinish, Wait(70), Paint(3), JobsFinish, Wait(70)], &|lab| {
        let file = lab.hdr.ok_or("no merged photo")?;
        let (recipe, key) = lab.recipe().ok_or("no recipe")?;
        let settings = rawdev::merged::parse_settings(&recipe.settings)?;
        let area = settings.regions.first().ok_or("no area was painted")?.clone();
        let k = lab.frames.iter().position(|f| f.0 == recipe.sources[area.frame].file).ok_or("the frame of the area is not a frame")?;
        // the frame of the area is not reachable: the tool takes no further area
        lab.act(SourceGone(k));
        lab.step(100, Wait(35))?;
        lab.step(101, Paint(20))?;
        lab.step(102, JobsFinish)?;
        lab.step(103, Wait(70))?;
        let (_, key2) = lab.recipe().ok_or("no recipe")?;
        if key2 != key {
            return Err(format!("a change of the recipe was stored while a source is gone (status \"{}\")", lab.status()));
        }
        // the computation itself, as a job or an export that got through would run it: one more area of that frame
        let mut changed = settings.clone();
        changed.regions.push(hdr::regions::Area { frame: area.frame, strokes: vec![hdr::regions::AreaStroke { erase: false, radius: 0.05, points: vec![[0.3, 0.6], [0.35, 0.6]] }] });
        let src = rawdev::merged::source_for(file, &changed).ok_or("no source")?;
        let cancel = AtomicBool::new(false);
        match rawdev::merged::compute(&src, &cancel, &|_, _| {}, &mut |_, _| {}) {
            Err(e) if e.starts_with(rawdev::merged::MISSING) => {}
            other => return Err(format!("merging an area whose frame is gone: {:?}", other.map(|_| "computed"))),
        }
        while rawdev::merged::cache_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if rawdev::merged::cached(&src.key) || rawdev::merged::preview_cached(&src.key) {
            return Err("something was stored for the result that could not be computed".into());
        }
        // with the source back: computed, and the same as a fresh merge of that recipe
        lab.act(SourceBack);
        rawdev::merged::compute(&src, &cancel, &|_, _| {}, &mut |_, _| {})?;
        let stored = rawdev::merged::read_full(&src.key).ok_or("result not readable")?;
        let sources: Vec<PathBuf> = recipe.sources.iter().map(|s| lab.frames.iter().find(|f| f.0 == s.file).unwrap().2.clone()).collect();
        let fresh = rawdev::merged::MergedSource::new(910_000_000 + file, sources, changed, format!("f{}-{:016x}", 910_000_000 + file, 1), format!("f{}-{:016x}", 910_000_000 + file, 2));
        rawdev::merged::compute(&fresh, &cancel, &|_, _| {}, &mut |_, _| {})?;
        let want = rawdev::merged::read_full(&fresh.key).ok_or("fresh merge not readable")?;
        while rawdev::merged::cache_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        rawdev::merged::forget_file(fresh.file);
        let differing = stored.px.iter().zip(&want.px).filter(|(x, y)| (0..3).any(|c| (x[c] - y[c]).abs() > 2e-3 * (1.0 + x[c].abs()))).count();
        if stored.px.len() != want.px.len() || differing > 0 {
            return Err(format!("the stored full resolution differs from a merge of its recipe in {differing} of {} pixels", want.px.len()));
        }
        Ok(())
    });
}

/// F6: ⌫ / "In den Papierkorb legen …" and "Abgelehnte Fotos löschen" with a merged photo among the photos: it is
/// skipped with the reason "Zustand unbekannt" (shown with its internal name "….dng"). Nothing says that a merged
/// photo has no file and leaves through "HDR entfernen (Quellen bleiben)".
#[test]
fn deleting_a_merged_photo_names_the_reason() {
    let p = crate::delete::plan(vec![crate::delete::Candidate { image: 1, file: 1, path: PathBuf::from("/not/there/IMG_2-HDR.dng"), state: library::merge::STATUS_VIRTUAL as u8, copy: false }]);
    assert!(p.disk.is_empty() && p.missing.is_empty(), "a merged photo is never deleted as a file");
    assert!(p.skipped.iter().all(|(_, why)| why != crate::tr("Zustand unbekannt")), "skipped as {:?}", p.skipped);
}

/// F7 (found through stress seed 88000119): ⌘Z while a new merge computes its first result waits for the job. When
/// the job ends and the photo carries work by then, the question "… entfernen?" opens – and its button "HDR entfernen"
/// undoes "the newest step" of the library, whatever that is by now. A step made in between (here: one of the quick
/// collection) is taken back instead, the merged photo stays.
#[test]
fn confirming_the_removal_undoes_the_step_of_that_photo() {
    use Act::*;
    passes_then(&[Create(0b0011100), Undo, Reference(0), Paint(3)], &|lab| {
        let file = lab.hdr.ok_or("no merged photo")?;
        if !lab.a.lib.manage.merge_undo_waiting.contains(&file) {
            return Err("⌘Z does not wait for the first computation".into());
        }
        // another step of the library made meanwhile
        lab.a.lib.manage.push(library_manage::Undo::Quick { label: "other step".into(), images: Vec::new() });
        lab.step(100, JobsFinish)?;
        // (no further action: a step of the model would answer the question first, as a user has to)
        for _ in 0..5 {
            lab.frame();
        }
        lab.settle();
        let Some(library_manage::Ask::RemoveMerge { files, undo: true, .. }) = &lab.a.lib.manage.ask else {
            return Err(format!("no question about the merged photo (in the library: {:?})", lab.merged_files()));
        };
        if files != &vec![file] {
            return Err(format!("the question is about {files:?}"));
        }
        // "HDR entfernen" in the dialog (what its button does)
        lab.answer_ask(true);
        lab.settle();
        for _ in 0..5 {
            lab.frame();
        }
        lab.settle();
        let other_left = lab.a.lib.manage.undo.iter().any(|u| matches!(u, library_manage::Undo::Quick { label, .. } if label == "other step"));
        let there = lab.merged_files().contains(&file);
        lab.hdr = there.then_some(file);
        if there || !other_left {
            return Err(format!("\"HDR entfernen\" confirmed: the merged photo is {}, the other step was {}", if there { "still there" } else { "gone" }, if other_left { "kept" } else { "undone instead" }));
        }
        Ok(())
    });
}

/// QA window run 9, M2: "Abbrechen" while the full resolution of a new merge is computed keeps the photo with its
/// preview – and it stays that way. Before, the full resolution was merged "ahead" two seconds later, unasked, while
/// the status line still said "HDR abgebrochen".
#[test]
fn a_cancelled_full_resolution_is_not_computed_unasked() {
    use Act::*;
    passes_then(&[Create(0b0011100), JobStep(0), Cancel, JobsFinish, Wait(35), Wait(70), Wait(70), Wait(70)], &|lab| {
        let file = lab.hdr.ok_or("the merged photo is gone (it showed its preview: cancelling keeps it)")?;
        let (_, key) = lab.recipe().ok_or("no recipe")?;
        if !rawdev::merged::preview_cached(&key) {
            return Err("premise: the preview was not there when the job was cancelled".into());
        }
        let started = crate::hdr_ui::gate::started();
        for n in 0..6 {
            lab.step(100 + n, Wait(70))?;
        }
        if rawdev::merged::cached(&key) || crate::hdr_ui::gate::started() != started || crate::hdr_ui::job_alive(file) {
            return Err(format!("after \"Abbrechen\" the full resolution was computed anyway (status \"{}\")", lab.status()));
        }
        let line = lab.status();
        // (the preview in place with its full resolution stopped, or – cancelled before the preview was put in place –
        // the reference still standing in: both say that the computation was cancelled)
        if !(line.starts_with("Vorschau aktuell · volle Auflösung abgebrochen") || line.starts_with("Nicht aktuell")) {
            return Err(format!("status \"{line}\""));
        }
        // asked for (a print, the contact sheet: `jobs::load_for_output`): computed as well (cross-check N3: the print
        // failed with "cancelled", because its document did not count as asked for)
        {
            let path = lab.hdr_path.clone().ok_or("no path")?;
            let l = crate::jobs::load_for_output(&path, &crate::Settings::default(), || true)?.ok_or("print: not loaded")?;
            let d = l.previews.foreign.clone().ok_or("print: no document")?;
            d.render_full(&crate::Settings::default()).map_err(|e| format!("print after the cancel: {e}"))?;
            // (the result is written to the cache in the background)
            while rawdev::merged::cache_busy() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            if !rawdev::merged::cached(&key) {
                return Err("the print did not compute the full resolution".into());
            }
            // (again without the result, for the export below)
            rawdev::merged::forget_full(&key);
        }
        // asked for (an export): computed, and the tool says so
        match rawdev::RawDoc::open_for_output(&lab.hdr_path.clone().ok_or("no path")?, &crate::Settings::default(), &|| true) {
            Ok(Some(d)) => d.render_full(&crate::Settings::default()).map(|_| ()).map_err(|e| format!("export after the cancel: {e}"))?,
            other => return Err(format!("export after the cancel: {:?}", other.map(|_| ()))),
        }
        // (the result is written to the cache in the background)
        while rawdev::merged::cache_busy() {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        if !rawdev::merged::cached(&key) {
            return Err("the export did not compute the full resolution".into());
        }
        Ok(())
    });
}
