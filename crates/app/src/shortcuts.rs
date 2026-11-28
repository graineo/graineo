//! Central table of all keyboard shortcuts: source for the overview (⌘/), the Mac menu (shortcuts
//! and titles) and new queries ([`pressed`]). A test checks that no key is bound twice in
//! overlapping contexts.
//!
//! Rules: existing single keys stay unchanged; new
//! overlays of the sensor view only with ⌥ + letter (physical key, so that ⌥C is not lost
//! as "ç").

use eframe::egui;
use egui::Key;
use crate::menu::i18n::{self,Text};

/// Where a shortcut applies. `SingleImage` includes the sensor view (it is a view of it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Everywhere,
    Raster,
    SingleImage,
    Sensor,
    /// the photo comparison (photo_compare_ui.rs) in the library and in Develop: own keys while it is open, before
    /// those of the grid and the single image (it takes the key events it handles)
    Compare,
    /// the print room (print_room.rs): it takes the whole window and switches the other shortcuts off
    PrintRoom,
    /// the slideshow (slideshow.rs): it takes the whole window and all keys
    Slideshow,
}

impl Context {
    pub fn title(self) -> &'static str {
        match self {
            Context::Everywhere => i18n::text(Text::Everywhere),
            Context::Raster => i18n::text(Text::Grid),
            Context::SingleImage => i18n::text(Text::SinglePhoto),
            Context::Sensor => i18n::text(Text::SensorView),
            Context::Compare => crate::tr("Vergleich"),
            Context::PrintRoom => crate::tr("Druckraum"),
            Context::Slideshow => crate::tr("Diashow"),
        }
    }

    /// Can both apply at the same time?
    pub fn overlaps(self, o: Context) -> bool {
        use Context::*;
        match (self, o) {
            // while the print room is open no other shortcut acts (main.rs: `dialog_open`)
            (PrintRoom, PrintRoom) => true,
            (PrintRoom, _) | (_, PrintRoom) => false,
            (Slideshow, Slideshow) => true,
            (Slideshow, _) | (_, Slideshow) => false,
            (Everywhere, _) | (_, Everywhere) => true,
            (a, b) if a == b => true,
            (SingleImage, Sensor) | (Sensor, SingleImage) => true,
            _ => false,
        }
    }
}

/// Modifiers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mods {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
}

const N: Mods = Mods { cmd: false, shift: false, alt: false };
const C: Mods = Mods { cmd: true, shift: false, alt: false };
const CS: Mods = Mods { cmd: true, shift: true, alt: false };
const CA: Mods = Mods { cmd: true, shift: false, alt: true };
const CSA: Mods = Mods { cmd: true, shift: true, alt: true };
const S: Mods = Mods { cmd: false, shift: true, alt: false };
const A: Mods = Mods { cmd: false, shift: false, alt: true };
const SA: Mods = Mods { cmd: false, shift: true, alt: true };

/// Trigger: a key (with modifiers) or a mouse gesture (overview only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    KeyPress(Mods, Key),
    /// several keys with the same effect (e.g. stars 1…5) – as text for the overview
    Shortcuts(Mods, &'static [Key]),
    Geste(&'static str),
}

pub struct Shortcut {
    pub id: &'static str,
    pub context: Context,
    pub trigger: Trigger,
    pub effect: &'static str,
}

const fn t(id: &'static str, context: Context, m: Mods, k: Key, effect: &'static str) -> Shortcut {
    Shortcut { id, context, trigger: Trigger::KeyPress(m, k), effect }
}
const fn ts(id: &'static str, context: Context, m: Mods, k: &'static [Key], effect: &'static str) -> Shortcut {
    Shortcut { id, context, trigger: Trigger::Shortcuts(m, k), effect }
}
const fn g(id: &'static str, context: Context, text: &'static str, effect: &'static str) -> Shortcut {
    Shortcut { id, context, trigger: Trigger::Geste(text), effect }
}

use Context::{SingleImage as E, Raster as R, Sensor as SE, Everywhere as U, Compare as CM, PrintRoom as PR, Slideshow as SL};

/// Opens the "Retouch" tool of the rail.
pub const RETOUCH: &str = "tool_retouch";

/// All shortcuts.
pub const ALL: &[Shortcut] = &[
    // everywhere
    t("tab_info", U, C, Key::I, "rechte Leiste: Reiter „Info“"),
    t("tab_info_grid", R, N, Key::I, "rechte Leiste: Reiter „Info“"),
    t("tab_edit", U, N, Key::E, "rechte Leiste: Reiter „Bearbeiten“"),
    t("export", U, CS, Key::E, "Exportieren (Dialog)"),
    t("print", U, C, Key::P, "Drucken (Druckraum)"),
    // the comparison keeps ⌘↩ for itself while it is open (it takes its keys first)
    t("slideshow", U, C, Key::Enter, "Diashow: Auswahl, sonst alle Fotos der Ansicht"),
    t("merge_hdr", U, CS, Key::H, "HDR zusammenfügen (Dialog)"),
    t("import", U, CS, Key::I, "Importieren (Speicherkarte oder Ordner)"),
    t("open_folder", U, C, Key::O, "Ordner ansehen, ohne zu importieren"),
    // ⌘R turns the photo and F5 is the top bar: ⌥⌘R (Ctrl+Alt+R on Windows) reads a viewed folder again
    t("refresh_folder", U, CA, Key::R, "angesehenen Ordner neu einlesen"),
    t("settings", U, C, Key::Comma, "Einstellungen"),
    // only while the settings are open (modal window); takes ⌘F before the library's search field
    t("prefs_search", U, C, Key::F, "Einstellungen durchsuchen"),
    t("overview", U, C, Key::Slash, "diese Übersicht"),
    // photo zoom in Develop and in the comparison (main.rs `zoom_shortcuts`); in the darkroom ⌘+ / ⌘− stay with the grain loupe
    t("image_zoom_in", U, C, Key::Plus, "Foto vergrößern (in der Dunkelkammer: Körnungs-Lupe)"),
    t("image_zoom_in_us", U, C, Key::Equals, "Foto vergrößern (US-Tastatur)"),
    t("image_zoom_out", U, C, Key::Minus, "Foto verkleinern (in der Dunkelkammer: Körnungs-Lupe)"),
    t("image_zoom_fit", U, C, Key::Num0, "Foto einpassen"),
    // UI size in steps 90 … 200 % (prefs.rs). With ⇧ the keys arrive differently per layout (German ⇧+ = *, ⇧- = _,
    // ⇧0 = =): without a logical key egui reports the physical one, so the German positions are listed as well
    t("ui_larger", U, CS, Key::Plus, "Oberfläche vergrößern"),
    t("ui_larger_us", U, CS, Key::Equals, "Oberfläche vergrößern (US-Tastatur)"),
    t("ui_larger_de", U, CS, Key::CloseBracket, "Oberfläche vergrößern (deutsche Tastatur)"),
    t("ui_smaller", U, CS, Key::Minus, "Oberfläche verkleinern"),
    t("ui_smaller_de", U, CS, Key::Slash, "Oberfläche verkleinern (deutsche Tastatur)"),
    t("ui_reset", U, CS, Key::Num0, "Oberfläche auf 100 %"),
    t("show_in_finder", U, CS, Key::F, "Im Finder zeigen"),
    t("show_in_library_folder", U, CA, Key::F, "Im Ordner der Bibliothek anzeigen"),
    t("side_panels", U, N, Key::Tab, "Seitenleisten ein/aus"),
    t("all_panels", U, S, Key::Tab, "alle Bedienfelder ein/aus"),
    ts("stars", U, N, &[Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5], "Sterne löschen / setzen (⇧: danach nächstes Bild)"),
    ts("label", U, N, &[Key::Num6, Key::Num7, Key::Num8, Key::Num9], "Farbetikett Rot / Gelb / Grün / Blau"),
    t("pick", U, N, Key::P, "pick"),
    t("reject", U, N, Key::X, "reject"),
    t("unflag", U, N, Key::U, "Markierung aufheben"),
    t("quick", U, N, Key::B, "zur Schnellsammlung hinzufügen / daraus entfernen"),
    t("quick_show", U, C, Key::B, "Schnellsammlung zeigen"),
    t("quick_save", U, CA, Key::B, "Schnellsammlung als Sammlung sichern …"),
    t("quick_clear", U, CS, Key::B, "Schnellsammlung leeren …"),
    t("arrange_keywords", R, CS, Key::K, "Stichwörter ordnen … (Bibliothek)"),
    t("undo", U, C, Key::Z, "Rückgängig"),
    t("redo", U, CS, Key::Z, "Wiederholen"),
    t("copy_photo", U, C, Key::C, "Foto kopieren (Zwischenablage, z. B. zum Einfügen in eine Mail; in Textfeldern: Text kopieren)"),
    t("copy_settings", U, CS, Key::C, "Entwicklungseinstellungen kopieren"),
    t("paste_settings", U, CS, Key::V, "Entwicklungseinstellungen einfügen (auf alle ausgewählten Fotos)"),
    t("sync_settings", U, CSA, Key::V, "Einstellungen vom Haupt-Foto auf die Auswahl übertragen"),
    t("reset_all", U, CSA, Key::R, "Alle Einstellungen zurücksetzen (⌘Z nimmt es zurück)"),
    t("auto_tone", U, C, Key::U, "Automatisch: Tonwerte aus dem Bild"),
    t("virtual_copy", U, C, Key::Quote, "virtuelle Kopie anlegen"),
    t("new_collection", U, C, Key::N, "Neue Sammlung …"),
    t("quit", U, C, Key::Q, "graineo beenden (sichert die Einstellungen des Fotos)"),
    t("trash_rejected", U, CA, Key::Backspace, "abgelehnte Fotos in den Papierkorb"),
    t("trash_selected", U, C, Key::Backspace, "ausgewählte Fotos in den Papierkorb (mit Rückfrage), auch in einer Sammlung"),
    ts("delete_selected", U, N, &[Key::Backspace, Key::Delete], "ausgewählte Fotos löschen (Papierkorb, mit Rückfrage); in einer Sammlung nur aus der Sammlung entfernen, die Datei bleibt"),
    // Grid
    t("single_photo", R, N, Key::Enter, "Einzelbild öffnen (auch Leertaste, D, Doppelklick); auf einer zugeklappten Serie: aufklappen"),
    t("single_photo_space", R, N, Key::Space, "Einzelbild öffnen"),
    t("single_photo_d", R, N, Key::D, "Einzelbild öffnen"),
    t("select_all", R, C, Key::A, "alle auswählen"),
    t("select_none", R, C, Key::D, "Auswahl aufheben"),
    t("search", R, C, Key::F, "Suchfeld der Bibliothek"),
    t("jump_to_selection", R, N, Key::C, "zur Auswahl springen (aktives Foto in die Mitte des Rasters)"),
    g("grid_arrows", R, "← → ↑ ↓", "aktives Bild wechseln"),
    t("move_collection_up", R, CA, Key::ArrowUp, "gezeigte Sammlung in ihrer Ebene nach oben"),
    t("move_collection_down", R, CA, Key::ArrowDown, "gezeigte Sammlung in ihrer Ebene nach unten"),
    g("grid_click", R, "⌘-/⇧-Klick", "Auswahl erweitern / Bereich"),
    g("grid_marquee", R, "⇧-Ziehen", "Aufziehrahmen von überall (auch auf einem Foto): Fotos zur Auswahl hinzufügen"),
    g("people_yes", R, "↩", "Personen: Ja – Vorschlag annehmen bzw. Stapel öffnen"),
    g("people_no", R, "⌫", "Personen: Nein – Vorschlag ablehnen bzw. Name entfernen"),
    g("people_name", R, "N", "Personen: anderen Namen eintragen, Gruppe benennen, Person umbenennen"),
    g("people_photo", R, "D", "Personen: Foto des gewählten Gesichts in Entwickeln öffnen (auch Doppelklick aufs große Foto; zurück mit Esc)"),
    g("people_large", R, "Leertaste", "Personen: Foto groß zeigen und zurück zum Raster (auch Doppelklick auf ein Gesicht; Esc schließt)"),
    t("people_frames", R, N, Key::H, "Personen: Gesichtsrahmen im großen Foto ein/aus"),
    g("people_back", R, "Esc", "Personen: zurück aus einem geöffneten Stapel bzw. aus dem großen Foto"),
    t("stack_toggle", R, N, Key::S, "Serie/Stapel des aktiven Fotos auf-/zuklappen"),
    ts("stack_step", R, A, &[Key::ArrowLeft, Key::ArrowRight], "durch die Fotos der Serien gehen: eine zugeklappte Serie öffnet sich und schließt sich wieder, wenn man sie verlässt"),
    t("stack_toggle_all", U, S, Key::S, "alle Serien und Stapel auf-/zuklappen"),
    t("stack_group", R, C, Key::G, "Auswahl zu einem Stapel bündeln (Titelfoto = aktives Foto)"),
    t("stack_dissolve", R, CS, Key::G, "Serie/Stapel auflösen (die Fotos werden nie wieder zusammengelegt)"),
    t("stack_review", R, N, Key::N, "Vergleichen: die ausgewählten Fotos bzw. die ganze Serie des aktiven Fotos"),
    t("stack_pick", R, A, Key::P, "ganze Serie als Auswahl markieren"),
    t("stack_reject", R, A, Key::X, "ganze Serie ablehnen"),
    t("stack_unflag", R, A, Key::U, "Markierung der ganzen Serie aufheben"),
    ts("stack_stars", R, A, &[Key::Num0, Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5], "Sterne der ganzen Serie löschen / setzen"),
    ts("stack_label", R, A, &[Key::Num6, Key::Num7, Key::Num8, Key::Num9], "Farbetikett der ganzen Serie"),
    g("pairs_switch", R, "← →", "RAW+JPEG doppelt prüfen: voriges / nächstes Paar"),
    g("pairs_check", R, "Leertaste", "RAW+JPEG doppelt prüfen: JPEG anhaken / abhaken"),
    g("pairs_list", R, "Esc", "RAW+JPEG doppelt prüfen: zurück zur Liste"),
    // Photo comparison (photo_compare_ui.rs): in the library and in Develop, before the keys of grid and single image
    t("cmp_mode_compare", CM, N, Key::V, "Modus Vergleich (Felder nebeneinander)"),
    t("cmp_mode_tournament", CM, S, Key::T, "Modus Turnier (Duelle bis Platz 1–3)"),
    t("cmp_arrange", CM, N, Key::G, "Anordnung weiterschalten: Auto · 1 Reihe · 1 Spalte · 2 × 2 · Gewähltes groß · Alle"),
    t("cmp_zoom", CM, N, Key::Z, "Einpassen ↔ 100 % (an Fokuspunkt, Gesicht oder schärfster Stelle)"),
    t("cmp_zoom_200", CM, S, Key::Z, "200 %"),
    g("cmp_space", CM, "Leertaste halten", "100 % an der Maus, solange gehalten"),
    t("cmp_coupled", CM, N, Key::L, "Zoom und Verschieben gekoppelt an/aus"),
    g("cmp_single", CM, "⇧-Ziehen", "ein Feld einzeln verschieben"),
    t("cmp_align", CM, N, Key::A, "Ausrichten: Einzel-Versätze zurücksetzen"),
    t("cmp_fit", CM, S, Key::G, "Füllen ↔ ganz zeigen: Fotos füllen ihr Feld oder stehen ganz darin"),
    t("cmp_focus_point", CM, S, Key::F, "Fokuspunkt der Kamera in allen Feldern"),
    t("cmp_peaking", CM, N, Key::K, "Kanten zeigen (Fokus-Peaking, gleiche Schwelle in allen Feldern)"),
    t("cmp_clipping", CM, N, Key::J, "Clipping in allen Feldern"),
    t("cmp_exif", CM, N, Key::I, "Aufnahme-Zeile (Abweichungen hervorgehoben)"),
    t("cmp_measure", CM, N, Key::M, "Messen: Rechteck ziehen, Schärfewert je Feld"),
    t("cmp_faces", CM, N, Key::H, "Gesichterleiste ein/aus"),
    t("cmp_reference", CM, N, Key::R, "fokussiertes Foto als Referenz festhalten / aufheben"),
    t("cmp_blink", CM, N, Key::W, "Blinken: Referenz und Kandidat abwechselnd (nochmal: nebeneinander)"),
    t("cmp_wipe", CM, S, Key::W, "Wischer zwischen Referenz und Kandidat (nochmal: nebeneinander)"),
    t("cmp_difference", CM, A, Key::W, "Differenzbild zwischen Referenz und Kandidat (nochmal: nebeneinander)"),
    ts("cmp_field", CM, A, &[Key::Num1, Key::Num2, Key::Num3, Key::Num4], "Feld 1–4 fokussieren"),
    g("cmp_arrows", CM, "← →", "fokussiertes Feld bzw. Kandidat wechseln; im Turnier: linkes / rechtes Foto behalten"),
    t("cmp_tie", CM, N, Key::Equals, "Turnier: gleich gut (beide bleiben)"),
    t("cmp_both_out", CM, N, Key::Minus, "Turnier: beide raus"),
    g("cmp_last_duel", CM, "⌘Z", "Turnier: letztes Duell zurücknehmen"),
    t("cmp_choose", CM, N, Key::Enter, "fokussiertes Foto als bestes wählen (Titelfoto + Markierung „Auswahl“)"),
    t("cmp_rest", CM, S, Key::X, "Rest ablehnen: alle Fotos außer den markierten (ein ⌘Z)"),
    t("cmp_develop", CM, N, Key::D, "fokussiertes Foto in Entwickeln öffnen"),
    t("cmp_finish", CM, C, Key::Enter, "Serie als gesichtet abschließen, weiter zur nächsten ungesichteten"),
    t("cmp_back", CM, N, Key::Escape, "Vergleich beenden (im Turnier: anhalten)"),
    // Single image
    t("grid", E, N, Key::G, "zurück ins Raster (auch Esc)"),
    t("compare_open", E, N, Key::N, "Vergleichen: die im Filmstreifen ausgewählten Fotos"),
    t("grid_esc", E, N, Key::Escape, "zurück ins Raster (wenn kein Werkzeug offen ist)"),
    t("tab_edit_d", E, N, Key::D, "rechte Leiste: Reiter „Bearbeiten“"),
    t("info_overlay", E, N, Key::I, "Bildinfos im Bild: kurz → ausführlich → aus"),
    t("pin_histogram", U, S, Key::H, "Histogramm oben festhalten (Bearbeiten, Masken) ein/aus"),
    t("spinner_info", U, A, Key::I, "Rechenschritt am Kreisel immer zeigen (an/aus, auch ⌃⌥I)"),
    g("arrows", E, "← / →", "vorheriges / nächstes Bild"),
    ts("stack_step_single", E, A, &[Key::ArrowLeft, Key::ArrowRight], "vorheriges / nächstes Bild mit gleichem Zoom, auch in zugeklappte Serien hinein (sie schließen sich wieder, wenn man sie verlässt)"),
    t("stack_toggle_single", E, N, Key::S, "Serie/Stapel des aktiven Fotos auf-/zuklappen (Filmstreifen)"),
    t("stack_open_enter", E, N, Key::Enter, "zugeklappte Serie im Filmstreifen aufklappen (wie Doppelklick)"),
    t("zoom", E, N, Key::Z, "Einpassen ↔ 100 % (auch Leertaste)"),
    t("zoom_space", E, N, Key::Space, "Einpassen ↔ 100 %"),
    t("before", E, N, Key::Backslash, "Standardentwicklung ↔ Aktuell (linke Seite wählbar)"),
    t("overlays", E, N, Key::H, "Retusche-Punkte und Maskenkonturen ein/aus (in den Werkzeugen „Masken“ und „Retusche“ sichtbar)"),
    t("tool_retouch", E, N, Key::Q, "rechte Leiste: Werkzeug „Retusche“"),
    t("mask_overlay", E, S, Key::O, "Überlagerung der Maske: an, Darstellung wechseln, aus (im Werkzeug „Masken“)"),
    g("mask_delete", E, "⌫ / Entf", "gewählte Maske löschen (im Werkzeug „Masken“, statt des Fotos)"),
    t("brush_smaller", E, N, Key::OpenBracket, "Pinsel kleiner"),
    t("brush_larger", E, N, Key::CloseBracket, "Pinsel größer"),
    t("brush_softer", E, S, Key::OpenBracket, "Pinsel weicher"),
    t("brush_harder", E, S, Key::CloseBracket, "Pinsel härter"),
    g("brush_erase", E, "⌥ beim Malen", "Radiert aus der Maske"),
    g("brush_line", E, "⇧-Klick beim Malen", "Gerade Linie vom letzten Punkt"),
    g("mask_esc", E, "Esc", "Werkzeug der Maske beenden (Pinsel, Griffe, Klick-Auswahl)"),
    g("retouch_select", E, "Klick auf einen Kreis", "Retusche-Punkt wählen; Ziehen verschiebt Punkt oder Quelle"),
    g("retouch_delete", E, "⌫ / Entf", "gewählten Retusche-Punkt löschen (statt des Fotos)"),
    g("retouch_esc", E, "Esc", "Retusche-Werkzeug und Auswahl beenden"),
    t("camera_jpeg", E, S, Key::R, "Referenz: Kamera-JPEG"),
    t("black_white", E, N, Key::V, "Farbe ↔ Schwarzweiß"),
    t("focus_frame", E, S, Key::F, "Fokusfeld der Kamera ein/aus (Z/Klick: 100 % an dieser Stelle)"),
    t("clipping", E, N, Key::J, "Clipping anzeigen (Sensor-Ansicht: RAW-Clipping)"),
    t("hdr_glow", E, S, Key::J, "HDR: leuchtende Bereiche zeigen (mit HDR-Bildschirm: normal ↔ leuchtend)"),
    t("eyedropper", E, N, Key::W, "Weißabgleich-Pipette"),
    t("eyedropper_smaller", E, N, Key::Minus, "Weißabgleich-Pipette: kleinere Probe"),
    t("eyedropper_larger", E, N, Key::Plus, "Weißabgleich-Pipette: größere Probe"),
    t("eyedropper_larger_us", E, N, Key::Equals, "Weißabgleich-Pipette: größere Probe (US-Tastatur)"),
    g("eyedropper_shift", E, "⇧-Klick mit der Pipette", "weitere Probe: Weißabgleich aus dem Mittel aller Proben, Pipette bleibt an"),
    t("loupe", E, N, Key::O, "Lupe am Mauszeiger (Scrollen: 100/200/400 %)"),
    t("sensor_field", E, N, Key::M, "Messfeld (Foveon: Rauschen/SNR je Schicht)"),
    t("assessment_frame", E, S, Key::B, "Beurteilungsrahmen: Foto auf Mittelgrau mit weißem Rand ein/aus"),
    t("lights_out", E, N, Key::L, "Licht aus: gedimmt → aus → normal"),
    t("full_screen", U, N, Key::F, "Vollbild (ganze App) ein/aus"),
    t("soft_proof", U, C, Key::Y, "Farbproof: Voller Umfang ↔ zuletzt gewählter Farbraum"),
    t("gamut_warning", U, CS, Key::Y, "Farbumfang-Warnung: Pixel außerhalb des Proof-Farbraums markieren"),
    t("crop", E, N, Key::R, "Freistellen & Ausrichten"),
    t("straighten", E, N, Key::A, "Ausrichten: Linie am Horizont ziehen"),
    t("curve_auto_brightness", E, S, Key::A, "Gradationskurve: Auto (Helligkeit)"),
    t("curve_auto_rgb", E, SA, Key::A, "Gradationskurve: Auto (RGB)"),
    t("rotate_left", U, C, Key::R, "90° nach links drehen"),
    t("rotate_right", U, CS, Key::R, "90° nach rechts drehen"),
    t("toolbar", E, N, Key::T, "Werkzeugleiste unter dem Bild ein/aus"),
    t("compare", E, N, Key::Y, "Standardentwicklung | Aktuell nebeneinander"),
    t("snapshot", E, S, Key::Y, "Vergleich: jetzigen Stand als linke Seite festhalten"),
    t("swap_sides", E, A, Key::Y, "Vergleich: linke und rechte Seite tauschen"),
    t("darkroom", E, S, Key::D, "Dunkelkammer ein/aus (dort: Tab nur Bild, Esc zurück)"),
    t("dk_zoom_plus", E, C, Key::Plus, "Dunkelkammer: nächste Körnungs-Lupenstufe"),
    t("dk_zoom_equals", E, C, Key::Equals, "Dunkelkammer: nächste Körnungs-Lupenstufe (US-Tastatur)"),
    t("dk_zoom_minus", E, C, Key::Minus, "Dunkelkammer: vorherige Körnungs-Lupenstufe"),
    t("dk_contact", E, N, Key::K, "Kontaktabzug der Dunkelkammer ein/aus (Esc schließt zuerst)"),
    t("f5", E, N, Key::F5, "obere Leiste ein/aus"),
    t("f6", E, N, Key::F6, "Filmstreifen ein/aus"),
    t("f7", E, N, Key::F7, "linke Leiste ein/aus"),
    t("f8", E, N, Key::F8, "rechte Leiste ein/aus"),
    g("single_click", E, "Einfachklick ins Bild", "Einpassen ↔ 100 % an dieser Stelle (ohne aktives Werkzeug)"),
    g("double_click", E, "Doppelklick ins Bild", "zurück zur Bibliothek (ohne aktives Werkzeug)"),
    g("trackpad", E, "Zwei Finger wischen / Mausrad", "vergrößertes Bild verschieben"),
    g("zoom_gesture", E, "Zusammenziehen / ⌘ oder ⌥ + Scrollen", "zoomen"),
    // Sensor view (overlays, ⌥ + physical key)
    t("sensor_clipping", SE, A, Key::C, "Overlay RAW-Clipping"),
    t("sensor_reserve", SE, A, Key::R, "Overlay JPEG-Reserve"),
    t("sensor_sharpness", SE, A, Key::F, "Overlay Schärfe / Fokus-Peaking"),
    t("sensor_moire", SE, A, Key::M, "Overlay Moiré / Falschfarben"),
    t("sensor_noise", SE, A, Key::N, "Overlay Rauschen"),
    t("sensor_defects", SE, A, Key::D, "Overlay Defektpixel"),
    t("sensor_lens", SE, A, Key::L, "Overlay Objektiv (Vignettierung, Gitter)"),
    t("sensor_mosaic", SE, A, Key::G, "Mosaik mit Pixelgitter (ab 400 %)"),
    t("sensor_demosaic", SE, A, Key::V, "Demosaik-Verfahren weiterschalten"),
    ts("sensor_demosaic_n", SE, A, &[Key::Num1, Key::Num2, Key::Num3, Key::Num4], "Demosaik: RCD / Markesteijn 3 / Markesteijn 1 / bilinear"),
    // Print room (filmstrip of the print)
    t("print_prev", PR, N, Key::ArrowLeft, "vorheriges Foto im Filmstreifen (die Vorschau zeigt seine Seite)"),
    t("print_next", PR, N, Key::ArrowRight, "nächstes Foto im Filmstreifen (die Vorschau zeigt seine Seite)"),
    ts("print_extend", PR, S, &[Key::ArrowLeft, Key::ArrowRight], "Auswahl im Filmstreifen erweitern / verkleinern"),
    t("print_select_all", PR, C, Key::A, "alle Fotos der Ansicht auswählen"),
    ts("print_deselect", PR, N, &[Key::Backspace, Key::Delete], "Haupt-Foto aus der Auswahl nehmen (nichts wird gelöscht)"),
    t("print_page_prev", PR, N, Key::PageUp, "vorige Seite"),
    t("print_page_next", PR, N, Key::PageDown, "nächste Seite"),
    t("print_close", PR, N, Key::Escape, "Druckraum schließen"),
    g("print_click", PR, "Klick / ⇧-Klick / ⌘-Klick im Filmstreifen", "Foto wählen / Bereich wählen / einzeln dazu oder weg"),
    // Slideshow (slideshow.rs): also stars, labels and P/X/U as everywhere
    t("slideshow_pause", SL, N, Key::Space, "Diashow anhalten / weiter"),
    t("slideshow_prev", SL, N, Key::ArrowLeft, "voriges Foto"),
    t("slideshow_next", SL, N, Key::ArrowRight, "nächstes Foto"),
    t("slideshow_info", SL, N, Key::I, "Infozeile ein/aus"),
    t("slideshow_end", SL, N, Key::Escape, "Diashow beenden"),
    // Sliders
    g("slider_double", U, "Regler: Doppelklick", "zurücksetzen"),
    g("slider_alt", U, "Regler: ⌥ beim Ziehen", "zehnmal feiner"),
    g("slider_arrows", U, "Regler: Maus darüber + ↑/↓", "verstellen (⇧ grob, ⌥ fein)"),
    g("group_alt", U, "Gruppe: ⌥-Klick auf Überschrift", "Gruppe zurücksetzen"),
    g("histogram", E, "Histogramm ziehen", "Tiefen / Belichtung / Lichter verstellen"),
];

pub fn find(id: &str) -> Option<&'static Shortcut> {
    ALL.iter().find(|k| k.id == id)
}

fn modifier_text(m: Mods) -> String {
    modifier_text_for(m, cfg!(target_os = "macos"))
}

fn modifier_text_for(m: Mods, mac: bool) -> String {
    // Windows and Linux: Ctrl instead of ⌘ (egui maps "command" to Ctrl there), modifier keys spelled out
    if !mac {
        let mut s = String::new();
        if m.cmd {
            s += crate::tr("Strg+");
        }
        if m.alt {
            s += crate::tr("Alt+");
        }
        if m.shift {
            s += crate::tr("Umschalt+");
        }
        return s;
    }
    // macOS order: ⌥ ⇧ ⌘ (as in the system menus)
    let mut s = String::new();
    if m.alt {
        s.push('⌥');
    }
    if m.shift {
        s.push('⇧');
    }
    if m.cmd {
        s.push('⌘');
    }
    s
}

pub use crate::menu::i18n::spell_modifiers;

fn is_modifier_symbol(c: char) -> bool {
    matches!(c, '⌘' | '⌃' | '⌥' | '⇧')
}

/// A display text for this platform: on macOS unchanged, on Windows and Linux the modifier symbols spelled out in
/// the current language ("Strg+Z", "Ctrl+Z"). `tr` applies it, so every translated text follows it.
pub fn platform_text(text: &str) -> std::borrow::Cow<'_, str> {
    if cfg!(target_os = "macos") || !text.chars().any(is_modifier_symbol) {
        return std::borrow::Cow::Borrowed(text);
    }
    let names = [crate::tr("Strg+"), crate::tr("Alt+"), crate::tr("Umschalt+")].map(|n| n.trim_end_matches('+'));
    std::borrow::Cow::Owned(spell_modifiers(text, names))
}

pub fn key_text(k: Key) -> String {
    match k {
        Key::Enter => "↩".into(),
        Key::Space => crate::tr("Leertaste").into(),
        Key::Escape => "Esc".into(),
        Key::Tab => "Tab".into(),
        Key::Backspace => "⌫".into(),
        Key::Backslash => "\\".into(),
        Key::OpenBracket => "[".into(),
        Key::CloseBracket => "]".into(),
        Key::Comma => ",".into(),
        Key::Slash => "/".into(),
        Key::Quote => "'".into(),
        Key::ArrowUp => "↑".into(),
        Key::ArrowDown => "↓".into(),
        Key::ArrowLeft => "←".into(),
        Key::ArrowRight => "→".into(),
        k => k.symbol_or_name().to_string(),
    }
}

/// Display text of a trigger, e.g. "⌘⇧E" or "1 … 5".
pub fn display_of(a: &Trigger) -> String {
    match a {
        Trigger::KeyPress(m, k) => format!("{}{}", modifier_text(*m), key_text(*k)),
        Trigger::Shortcuts(m, ks) => format!("{}{}", modifier_text(*m), ks.iter().map(|k| key_text(*k)).collect::<Vec<_>>().join(" ")),
        Trigger::Geste(t) => crate::tr(t).to_string(),
    }
}

/// Display text of a shortcut by its ID.
pub fn display(id: &str) -> String {
    find(id).map(|k| display_of(&k.trigger)).unwrap_or_default()
}

/// Display one member of a multi-key shortcut.
pub fn display_index(id: &str, index: usize) -> String {
    match find(id).map(|s| s.trigger) {
        Some(Trigger::Shortcuts(m, keys)) => keys.get(index).map(|k| display_of(&Trigger::KeyPress(m, *k))).unwrap_or_default(),
        _ => String::new(),
    }
}

/// Was the shortcut pressed this frame? Compares the *physical* key so that ⌥ shortcuts
/// do not fail on special characters (ç, ß …); modifiers must match exactly.
pub fn pressed(ctx: &egui::Context, id: &str) -> bool {
    let Some(k) = find(id) else { return false };
    let Trigger::KeyPress(m, key) = k.trigger else { return false };
    combo_pressed(ctx, m, key)
}

/// Which of the keys of a multi-key shortcut was pressed (index)?
pub fn which(ctx: &egui::Context, id: &str) -> Option<usize> {
    let k = find(id)?;
    let Trigger::Shortcuts(m, keys) = k.trigger else { return None };
    keys.iter().position(|&key| combo_pressed(ctx, m, key))
}

fn combo_pressed(ctx: &egui::Context, m: Mods, key: Key) -> bool {
    if ctx.egui_wants_keyboard_input() {
        return false;
    }
    ctx.input(|i| {
        i.events.iter().any(|e| match e {
            egui::Event::Key { key: k, physical_key, pressed: true, repeat: false, modifiers } => {
                (*k == key || *physical_key == Some(key)) && modifiers.command == m.cmd && modifiers.shift == m.shift && modifiers.alt == m.alt
            }
            _ => false,
        })
    })
}

/// Overview window (⌘/), generated from the table, grouped by context.
pub fn window(ctx: &egui::Context, open: &mut bool) {
    if !*open {
        return;
    }
    let height = ctx.content_rect().height() * 0.7;
    let a = crate::dialog::show(ctx, "tasten", &crate::trf!("{a} Tastenkürzel", a = ALL.len()), 520.0, |ui| {
        crate::dialog::text(ui, crate::tr("Einzelne Buchstaben wirken, solange kein Eingabefeld den Fokus hat. Die Liste entsteht aus derselben Tabelle wie das Menü."));
        egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
            for kx in [Context::Everywhere, Context::Raster, Context::Compare, Context::SingleImage, Context::Sensor, Context::PrintRoom, Context::Slideshow] {
                ui.add_space(6.0);
                ui.label(egui::RichText::new(kx.title()).strong());
                egui::Grid::new(("keys", kx.title())).num_columns(2).min_col_width(210.0).spacing([24.0, 3.0]).show(ui, |ui| {
                    for k in ALL.iter().filter(|k| k.context == kx) {
                        ui.label(egui::RichText::new(display_of(&k.trigger)).monospace());
                        ui.label(crate::tr(k.effect));
                        ui.end_row();
                    }
                });
            }
        });
    }, crate::dialog::Buttons::close());
    if a != crate::dialog::Answer::Open {
        *open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_combos(a: &Trigger) -> Vec<(Mods, Key)> {
        match a {
            Trigger::KeyPress(m, k) => vec![(*m, *k)],
            Trigger::Shortcuts(m, ks) => ks.iter().map(|k| (*m, *k)).collect(),
            Trigger::Geste(_) => Vec::new(),
        }
    }

    /// Deliberate overlaps: ⌘F searches the settings while they are open (modal), the library otherwise;
    /// in the darkroom ⌘+ / ⌘− zoom the grain loupe instead of the photo (main.rs `zoom_shortcuts`);
    /// German ⇧⌘0 arrives as ⇧⌘= with the physical key 0 (prefs::scale_shortcuts checks the reset first).
    const SHADOWED: &[(&str, &str)] = &[("prefs_search", "search"), ("image_zoom_in", "dk_zoom_plus"), ("image_zoom_in_us", "dk_zoom_equals"), ("image_zoom_out", "dk_zoom_minus"), ("slideshow", "cmp_finish")];

    #[test]
    fn no_duplicate_bindings() {
        let mut errors = Vec::new();
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                if !a.context.overlaps(b.context) || SHADOWED.contains(&(a.id, b.id)) {
                    continue;
                }
                for x in key_combos(&a.trigger) {
                    if key_combos(&b.trigger).contains(&x) {
                        errors.push(format!("{} ({:?}) und {} ({:?}): {}", a.id, a.context, b.id, b.context, display_of(&a.trigger)));
                    }
                }
            }
        }
        assert!(errors.is_empty(), "Bound twice:\n{}", errors.join("\n"));
    }

    #[test]
    fn pc_spelling_of_modifiers() {
        let n = ["Strg", "Alt", "Umschalt"];
        assert_eq!(spell_modifiers("⌘Z holt alles zurück.", n), "Strg+Z holt alles zurück.");
        assert_eq!(spell_modifiers("⇧⌘Y färbt", n), "Strg+Umschalt+Y färbt");
        assert_eq!(spell_modifiers("⌘-/⇧-Klick", n), "Strg-/Umschalt-Klick");
        assert_eq!(spell_modifiers("⌥ beim Ziehen: fein", n), "Alt beim Ziehen: fein");
        assert_eq!(spell_modifiers("auch ⌃⌥I", n), "auch Strg+Alt+I");
        assert_eq!(spell_modifiers("⌘+ und ⌘−", n), "Strg++ und Strg+−");
        assert_eq!(spell_modifiers("(⌥-Klick: alle)", n), "(Alt-Klick: alle)");
        assert_eq!(spell_modifiers("ohne Taste", n), "ohne Taste");
        assert_eq!(spell_modifiers("⌘Z", ["Ctrl", "Alt", "Shift"]), "Ctrl+Z");
    }

    #[test]
    fn modifier_text_per_platform() {
        let all = Mods { cmd: true, alt: true, shift: true };
        assert_eq!(modifier_text_for(all, true), "⌥⇧⌘");
        // German UI in tests: the keys themselves
        assert_eq!(modifier_text_for(all, false), "Strg+Alt+Umschalt+");
        assert!(!modifier_text_for(Mods { cmd: true, alt: false, shift: false }, false).contains('⌘'));
        #[cfg(not(target_os = "macos"))]
        assert_eq!(display("open_folder").chars().filter(|c| "⌘⌥⇧".contains(*c)).count(), 0);
    }

    #[test]
    fn ids_unique() {
        for (i, a) in ALL.iter().enumerate() {
            assert!(ALL[i + 1..].iter().all(|b| b.id != a.id), "duplicate ID: {}", a.id);
        }
    }
}
