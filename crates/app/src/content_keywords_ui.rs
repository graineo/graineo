//! Keyword suggestions from the picture content in the "Stichwörter" section of the info panel
//! (`library::content_keywords`): a row "Vorschläge" with chips. A click on a chip assigns the keyword to the shown
//! photo (one ⌘Z step), its × rejects the suggestion for this photo (remembered, also ⌘Z). The row is always there
//! (no jump when suggestions arrive); without the model or without an image vector it is dimmed and its tooltip says
//! why. Nothing is downloaded and nothing runs in bulk: suggestions are worked out for the shown photo only.

use super::*;
use library::content_keywords as ck;

/// Seconds after which the suggestions of the shown photo are worked out again (the search may have read the photo
/// meanwhile, a rejection may have been undone; a few small reads).
const RECHECK_S: u64 = 5;

#[derive(Clone)]
enum State {
    NoModel,
    Preparing,
    Failed(String),
    NoVector,
    Ready(Vec<ck::Suggestion>),
}

/// Suggestions of the shown photo, worked out once per photo (and again after a keyword change).
#[derive(Default)]
pub(super) struct ContentIdeas {
    shown: Option<(i64, State, std::time::Instant)>,
}

/// What the user did with a suggestion.
pub(super) enum Answer {
    /// assign the keyword (name in the interface language)
    Accept(String),
    /// reject: term id, shown name
    Reject(&'static str, String),
}

fn language() -> &'static str {
    crate::menu::i18n::locale::language().code()
}

impl LibraryView {
    /// Forget the worked-out suggestions (after keywords changed).
    pub(super) fn forget_content_ideas(&mut self) {
        self.content_ideas.shown = None;
    }

    fn content_state(&mut self, ctx: &egui::Context, image: i64) -> State {
        if let Some((i, s, at)) = &self.content_ideas.shown {
            let fresh = at.elapsed().as_secs() < RECHECK_S;
            if *i == image && fresh && !matches!(s, State::Preparing) {
                return s.clone();
            }
        }
        let c = ctx.clone();
        let s = match ck::vectors(move || c.request_repaint()) {
            ck::Vectors::NoModel => State::NoModel,
            ck::Vectors::Preparing => State::Preparing,
            ck::Vectors::Failed(e) => State::Failed(e),
            // a few hundred dot products and two small reads: fine in the frame, once per photo
            ck::Vectors::Ready(tv) => match self.db.as_ref().map(|d| ck::suggestions(d, image, &tv)) {
                Some(Ok(Some(v))) => State::Ready(v),
                Some(Ok(None)) | None => State::NoVector,
                Some(Err(e)) => State::Failed(e),
            },
        };
        self.content_ideas.shown = Some((image, s.clone(), std::time::Instant::now()));
        s
    }

    /// The row "Vorschläge" for the shown photo; returns what was clicked.
    pub(super) fn content_ideas_row(&mut self, ui: &mut egui::Ui, image: i64) -> Option<Answer> {
        let state = self.content_state(ui.ctx(), image);
        let mut answer = None;
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            let blocked = !matches!(state, State::Ready(_));
            let head = ui.label(egui::RichText::new(crate::tr("Vorschläge")).small().color(if blocked { Color32::from_gray(0x70) } else { lr::TEXT_DIM }));
            let why = match &state {
                State::NoModel => crate::tr("Vorschläge aus dem Bildinhalt brauchen das Suchmodell „Beste Treffer“ (SigLIP 2 Base). Installieren unter Einstellungen → Suche; es wird nichts von selbst geladen."),
                State::Preparing => crate::tr("Die Begriffe werden einmal für das Suchmodell vorbereitet (wenige Sekunden)."),
                State::Failed(_) => crate::tr("Die Vorschläge konnten nicht berechnet werden."),
                State::NoVector => crate::tr("Dieses Foto ist für das Suchmodell „Beste Treffer“ noch nicht eingelesen. Das geschieht im Hintergrund; danach erscheinen hier Vorschläge."),
                State::Ready(_) => crate::tr("Begriffe, die das Suchmodell im Bild erkennt. Klick übernimmt das Stichwort für dieses Foto, × lehnt den Vorschlag für dieses Foto ab. Nie automatisch vergeben."),
            };
            lr::hover_tip(ui, &head, why);
            match &state {
                State::Preparing => {
                    ui.add(egui::Spinner::new().size(10.0));
                }
                State::Ready(v) if v.is_empty() => {
                    ui.label(egui::RichText::new(crate::tr("keine sicheren")).small().color(lr::TEXT_DIM));
                }
                State::Ready(v) => {
                    for s in v {
                        let name = s.term.name(language());
                        match suggestion_chip(ui, name) {
                            Some(true) => answer = Some(Answer::Accept(name.to_string())),
                            Some(false) => answer = Some(Answer::Reject(s.term.id, name.to_string())),
                            None => {}
                        }
                    }
                }
                State::Failed(e) => {
                    let r = ui.label(egui::RichText::new("–").small().color(lr::TEXT_DIM));
                    lr::hover_tip(ui, &r, &crate::menu::i18n::tr_msg(e));
                }
                State::NoModel | State::NoVector => {
                    let r = ui.label(egui::RichText::new("–").small().color(Color32::from_gray(0x70)));
                    lr::hover_tip(ui, &r, why);
                }
            }
        });
        answer
    }

    /// Carry out an answer in the background (database, sidecars); ⌘Z takes it back.
    pub(super) fn content_ideas_answer(&mut self, image: i64, a: Answer) {
        self.forget_content_ideas();
        match a {
            Answer::Accept(name) => {
                let label = crate::trf!("Stichwort „{a}“ zugewiesen", a = name);
                self.in_background(
                    move |db| ck::accept(db, image, &name),
                    move |b, actions, r| {
                        match r {
                            Ok((n, snap)) => {
                                let text = crate::trf!("{a} ({n} Begleitdatei(en) geschrieben)", a = label, n = n);
                                actions.push(Action::Status(library_manage::keyword_status(&text, &snap)));
                                if !snap.is_empty() {
                                    b.manage.push(library_manage::Undo::Snap { label, snap, keywords: true, smart: false });
                                }
                            }
                            Err(e) => actions.push(Action::Status(crate::trf!("Stichwörter: {e}", e = e))),
                        }
                        b.forget_content_ideas();
                        b.reload_keywords();
                    },
                );
            }
            Answer::Reject(term, name) => {
                let label = crate::trf!("Vorschlag „{a}“ abgelehnt", a = name);
                self.in_background(
                    move |db| ck::reject(db, image, term),
                    move |b, actions, r| {
                        match r {
                            Ok(snap) => {
                                actions.push(Action::Status(label.clone()));
                                if !snap.is_empty() {
                                    b.manage.push(library_manage::Undo::Snap { label, snap, keywords: false, smart: false });
                                }
                            }
                            Err(e) => actions.push(Action::Status(crate::trf!("Stichwörter: {e}", e = e))),
                        }
                        b.forget_content_ideas();
                    },
                );
            }
        }
    }
}

/// A suggestion chip: outlined (not filled like an assigned keyword), the name assigns, × rejects.
/// Returns `Some(true)` for assign, `Some(false)` for reject.
fn suggestion_chip(ui: &mut egui::Ui, text: &str) -> Option<bool> {
    let g = ui.painter().layout_no_wrap(text.to_string(), egui::FontId::proportional(10.5), lr::TEXT);
    let (r, _) = ui.allocate_exact_size(g.size() + egui::vec2(24.0, 6.0), egui::Sense::hover());
    let body = egui::Rect::from_min_max(r.min, egui::pos2(r.right() - 16.0, r.bottom()));
    let cross = egui::Rect::from_min_max(egui::pos2(r.right() - 16.0, r.top()), r.max);
    let take = ui.interact(body, ui.scope_id().with(("content-idea", text)), egui::Sense::click());
    let no = ui.interact(cross, ui.scope_id().with(("content-idea-no", text)), egui::Sense::click());
    let p = ui.painter();
    if take.hovered() {
        p.rect_filled(r, 0.0, Color32::from_gray(0x33));
    }
    p.rect_stroke(r.shrink(0.5), 0.0, egui::Stroke::new(1.0, Color32::from_gray(if take.hovered() || no.hovered() { 0x80 } else { 0x55 })), egui::StrokeKind::Inside);
    p.galley(r.min + egui::vec2(7.0, 3.0), g, if take.hovered() { Color32::WHITE } else { lr::TEXT });
    p.text(cross.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(11.0), if no.hovered() { Color32::WHITE } else { lr::TEXT_DIM });
    lr::hover_tip(ui, &take, crate::tr("Als Stichwort für dieses Foto übernehmen"));
    lr::hover_tip(ui, &no, crate::tr("Vorschlag für dieses Foto ablehnen"));
    if take.clicked() {
        Some(true)
    } else if no.clicked() {
        Some(false)
    } else {
        None
    }
}
