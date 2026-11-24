//! Context help for the sliders: a floating
//! card with name, short explanation and – where there is something to see – a small
//! before/after example. The example images are pre-rendered and embedded
//! (`cargo run --release -p develop --features spp-gpu --example help_images`, from the
//! DP2 Merrill test images) so the card appears instantly without any computation.
//!
//! The existing short hints of the sliders (`hint`) remain the basis: the card shows them
//! as the first sentence and adds what the slider does exactly here and when you need it.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use eframe::egui;
use egui::Color32;

use crate::lr;

/// Before/after pair (280 × 190 pixels each, shown at 140 × 95 points).
pub struct Example {
    name: &'static str,
    before: &'static [u8],
    after: &'static [u8],
    /// Caption below the thumbnails
    before_label: &'static str,
    after_label: &'static str,
}

pub struct Card {
    /// First sentence, in case the slider has no short hint of its own
    short: &'static str,
    /// Addition: what exactly happens, when you need the slider
    text: &'static str,
    example: Option<Example>,
    /// drawn below the text (e.g. the field table of the search card)
    extra: Option<fn(&mut egui::Ui)>,
}

macro_rules! example_pair {
    ($name:literal, $before_label:expr, $after_label:expr) => {
        Some(Example {
            name: $name,
            before: include_bytes!(concat!("../assets/help/", $name, "_before.jpg")),
            after: include_bytes!(concat!("../assets/help/", $name, "_after.jpg")),
            before_label: $before_label,
            after_label: $after_label,
        })
    };
}

const fn card(short: &'static str, text: &'static str, example: Option<Example>) -> Card {
    Card { short, text, example, extra: None }
}

// ── Merged photos (tool "Zusammenfügen") ──
pub static HDR_FRAMES: Card = card(
    "Die Aufnahmen der Belichtungsreihe, nach Belichtung geordnet.",
    "• **Haken** – Aufnahme benutzen; ohne Haken bleibt sie gedimmt an ihrem Platz\n• **Stern** – Referenz: ihre Lage, Farbe und Belichtung gelten für das Ergebnis\n• **Maustaste halten** – diese Aufnahme groß zeigen (loslassen = zurück zum HDR)\n• **Foto aus dem Filmstreifen hierher ziehen** – als Aufnahme hinzufügen",
    None,
);

// ── Print room ──
static PRINT_RESOLUTION: Card = card(
    "Pixel je Zoll auf dem Papier.",
    "graineo rechnet jede Seite genau in dieser Auflösung, damit der Druckertreiber nicht mit einfacherem Verfahren nachskaliert. Epson-Drucker arbeiten intern mit 360 oder 720 ppi, viele Canon- und HP-Drucker mit 300 oder 600. Mehr als 360 ppi zeigt sich nur bei genauem Hinsehen und kostet Zeit und Speicher.",
    None,
);
static PRINT_SHARPEN: Card = card(
    "Ausgleich für die Unschärfe von Tinte und Papier.",
    "Wirkt nach dem Skalieren auf die Helligkeit, mit einem Radius in Millimetern auf dem Papier – bei jeder Auflösung gleich. Mattes Papier lässt die Tinte stärker verlaufen und bekommt mehr. Am Bildschirm wirkt die Schärfung zu kräftig; maßgeblich ist der Ausdruck.",
    None,
);
static PRINT_COLOR: Card = card(
    "Farbraum der Seite; die Farben rechnet der Drucker um.",
    "Die Seite trägt ihr Farbprofil (Adobe RGB oder sRGB). Im Druckdialog übernimmt der Treiber die Umrechnung auf Drucker und Papier – dort Papiersorte und Qualität wählen und die Farbanpassung des Druckers eingeschaltet lassen. Adobe RGB erhält kräftige Grün- und Cyantöne, die gute Tintenstrahldrucker drucken können.",
    None,
);

pub static LENS_DEFAULT: Card = card("", "Die Vorgabe gilt auch für künftige Importe. Ein Foto mit eigener Zuweisung behält sie.", None);

// ── Basic ──
static TEMPERATURE: Card = card("", "Zuerst Vorgabe oder Pipette wählen, dann hier nach Geschmack nachstellen. Die Helligkeit bleibt gleich, nur das Verhältnis von Rot zu Blau ändert sich.", example_pair!("temperature", "Vorher", "+0,70"));
static TINT: Card = card("", "Hilft, wenn Neutrales nach der Temperatur noch grünlich oder rosa wirkt – etwa unter Laub oder bei Leuchtstofflicht.", example_pair!("tint", "Vorher", "+0,50"));
static EXPOSURE: Card = card("", "Hier wird nicht einfach multipliziert: Die Lichter rollen auf der Kurve weich ab. Was der Sensor schon ausgebrannt hat, holt auch ein negativer Wert nicht zurück.", example_pair!("exposure", "Vorher", "+0,70 EV"));
static CONTRAST: Card = card("Steilheit der S-förmigen Tonkurve der Kamera.", "+ trennt Hell und Dunkel stärker, Tiefen und Lichter laufen früher zu; − wirkt flacher und offener. Der Kontrastzuschlag des Farbmodus (z. B. Vivid) kommt hinzu.", example_pair!("contrast", "Vorher", "+1,0"));
static HIGHLIGHTS: Card = card("Verschiebt das obere Ende der Tonkurve der Kamera.", "− dunkelt helle Bereiche ab und gibt Himmel und Wolken mehr Zeichnung, + hellt sie auf. Die Tiefen bleiben weitgehend unberührt.", example_pair!("highlights", "Vorher", "−1,0"));
static SHADOWS: Card = card("Hebt oder senkt den Fuß der Tonkurve der Kamera.", "+ öffnet dunkle Bildteile gleichmäßig im ganzen Bild, − macht sie satter. Für dunkle Bereiche neben hellen (Gegenlicht) ist Fill Light meist besser.", example_pair!("shadows", "Vorher", "+1,5"));
// ── Histogram, scopes, edit sections (replace the former hover texts) ──
pub static HISTOGRAM: Card = card(
    "Verteilung der Helligkeit im Bild, Rot, Grün und Blau getrennt.",
    "Links sind die Tiefen, rechts die Lichter. Die Dreiecke oben links und rechts leuchten hell, sobald Schatten oder Lichter abgeschnitten sind.\n• **Klick auf ein Dreieck** – zeigt das Clipping im Bild (J)\n• **Waagerecht ziehen** – verstellt Tiefen, Belichtung oder Lichter direkt; der Bereich unter dem Zeiger ist hell unterlegt",
    None,
);
pub static SCOPE_WAVE: Card = card(
    "Helligkeit über die Bildbreite.",
    "Links im Bild ist links in der Anzeige. Oben ist hell, unten dunkel.",
    None,
);
pub static SCOPE_PARADE: Card = card(
    "Rot, Grün und Blau nebeneinander, jeweils über die Bildbreite.",
    "Oben ist hell, unten dunkel. Gleich hohe Spuren an einer Stelle bedeuten neutrales Grau; ein Farbstich zeigt sich als Spur, die höher oder tiefer liegt als die anderen.",
    None,
);
pub static SCOPE_RGB_WAVE: Card = card(
    "Rot, Grün und Blau übereinander, über die Bildbreite.",
    "Links im Bild ist links in der Anzeige. Oben ist hell, unten dunkel; wo sich alle drei Farben decken, wird die Spur weiß (neutral).",
    None,
);
pub static VECTORSCOPE: Card = Card {
    short: "Zeigt, welche Farben im Bild stecken: Winkel = Farbton, Abstand von der Mitte = Sättigung.",
    text: "Jeder Bildpunkt landet an einer Stelle im Kreis. Die Richtung sagt, welche Farbe er hat (oben Rot, rechts Blau, unten Cyan, links Gelb), der Abstand von der Mitte, wie kräftig sie ist. Je heller ein Fleck, desto mehr Pixel haben diese Farbe.\n• **Mitte** – neutrales Grau, Weiß und Schwarz; ein Farbstich schiebt die ganze Wolke zur Seite\n• **Ringe** – Sättigung in Prozent (die Zahlen unten links)\n• **Kästchen** – Rot, Magenta, Blau, Cyan, Grün und Gelb bei 75 % Sättigung\n• **Haut** – die Hauttonlinie: Haut aller Menschen liegt nahe daran; weicht sie ab, hat die Haut einen Stich\n• **×1 · ×2 · ×4** – vergrößert die Mitte, denn die meisten Fotos sind nur wenig gesättigt\n• **S · M · L** – Größe der Anzeige im Kopf; ein Doppelklick auf die Anzeige wählt die nächste\n\nÜber der Anzeige steht oben links Farbton und Sättigung an der Zeigerposition.",
    example: None,
    extra: Some(vectorscope_sketch),
};

/// Small sketch in the vectorscope card: the colour circle in the orientation of the scope, with the two readings.
fn vectorscope_sketch(ui: &mut egui::Ui) {
    let size = 128.0;
    ui.add_space(6.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width().max(size), size), egui::Sense::hover());
    let c = egui::pos2(rect.left() + size / 2.0 + 4.0, rect.center().y);
    let r = size / 2.0 - 6.0;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(c, Color32::from_gray(0x80));
    let n = 72;
    for i in 0..=n {
        let a = i as f32 / n as f32 * std::f32::consts::TAU;
        let h = crate::vectorscope::hue_deg(a.cos() * 0.5, a.sin() * 0.5);
        mesh.colored_vertex(c + egui::vec2(a.cos(), -a.sin()) * r, Color32::from(egui::ecolor::Hsva::new(h / 360.0, 0.85, 0.8, 1.0)));
    }
    for i in 1..=n as u32 {
        mesh.add_triangle(0, i, i + 1);
    }
    let p = ui.painter();
    p.add(egui::Shape::mesh(mesh));
    p.circle_stroke(c, r, egui::Stroke::new(1.0, Color32::from_gray(0x50)));
    // saturation: an arrow from the centre outwards; hue: an arc along the rim
    let a = (-30f32).to_radians();
    let dir = egui::vec2(a.cos(), -a.sin());
    let white = egui::Stroke::new(1.5, Color32::WHITE);
    p.arrow(c, dir * (r - 4.0), white);
    let arc: Vec<egui::Pos2> = (0..=20).map(|k| {
        let t = (60.0 + k as f32 * 4.0).to_radians();
        c + egui::vec2(t.cos(), -t.sin()) * (r + 3.0)
    }).collect();
    p.add(egui::Shape::line(arc, white));
    let font = egui::FontId::proportional(10.5);
    let x = c.x + r + 14.0;
    p.text(egui::pos2(x, c.y - 24.0), egui::Align2::LEFT_CENTER, crate::tr("Winkel = Farbton"), font.clone(), lr::TEXT());
    p.text(egui::pos2(x, c.y - 8.0), egui::Align2::LEFT_CENTER, crate::tr("Abstand = Sättigung"), font.clone(), lr::TEXT());
    p.text(egui::pos2(x, c.y + 8.0), egui::Align2::LEFT_CENTER, crate::tr("Mitte = neutral"), font.clone(), lr::TEXT_DIM());
    p.text(egui::pos2(x, c.y + 24.0), egui::Align2::LEFT_CENTER, crate::tr("Helligkeit = Anzahl Pixel"), font, lr::TEXT_DIM());
}
/// Edit sections without a card of their own: the operation of the header comes from the controls line.
pub static EDIT_SECTION: Card = card(
    "Gruppe von Reglern zu einem Thema.",
    "Der Kopf lässt sich auf- und zuklappen. Sobald im Abschnitt etwas verstellt ist, erscheinen rechts im Kopf der Schalter und der Rücksetzknopf.",
    None,
);
// "Film" section (spektrafilm): sections, sliders and choice rows
pub static FILM_RENDERING: Card = card(
    "Kamera oder Film.",
    "• **Kamera** – Tonkurve und Farbe der Kamera\n• **Film** – wie auf Film belichtet, entwickelt und auf Papier kopiert, mit den Spektraldaten echter Filme\n\nDie Foveon-Rauschunterdrückung und -Schärfe laufen in beiden Fällen davor.",
    None,
);
static FILM_MATERIAL: Card = card(
    "Welcher Film belichtet wird.",
    "• **Farbnegativ** (Portra, Ektar, Gold …) – auf Papier kopiert; weich und verzeihend\n• **Dia** (Velvia, Kodachrome …) – direkt gescannt; kräftig, kontrastreich, wenig Spielraum\n• **Kino** (Vision3) – für die Filmkopie gemacht; T-Filme für Kunstlicht\n• **Schwarzweiß** – mit den Empfindlichkeiten des jeweiligen Films\n\n### Tipp\nDie Vorschaubilder zeigen dein Foto in jedem Film.",
    None,
);
static FILM_PAPER: Card = card(
    "Worauf das Negativ kopiert wird.",
    "Beim Farbnegativ entsteht das Bild erst im Abzug: Das Papier bestimmt Kontrast und Farbcharakter mit.\n• **Standard** – das Papier, für das der Film gedacht ist\n• **Negativ** – der entwickelte Film selbst (mit Orangemaske) statt des Abzugs\n\nDiafilm braucht kein Papier.",
    None,
);
static FILM_DARKROOM: Card = card(
    "Belichten und Kopieren wie im Labor.",
    "Die Filmbelichtung bestimmt, wo das Bild auf der Filmkurve liegt; der Abzug gleicht die Helligkeit aus. Abzugshelligkeit, Wärme und Tönung entsprechen Belichtungszeit und Farbfiltern im Vergrößerer.",
    None,
);
static FILM_EXPOSURE: Card = card("", "Über- oder Unterbelichten des Negativs in Blenden; der Abzug gleicht die Helligkeit aus.\n• **+** – satte, feinkörnige Bilder\n• **−** – flaue Schatten, gröberes Korn\n\nBei Dia und Negativ-Scan ändert sich die Helligkeit direkt.", None);
static FILM_PRINT: Card = card("", "Belichtungszeit im Vergrößerer in Blenden: + dunklerer, − hellerer Abzug.", None);
static FILM_WARMTH: Card = card("", "Gelbfilter im Vergrößerer (±1 ≈ ±15 CC): + wärmer, − kühler. Wirkt nur beim Abzug auf Papier.", None);
static FILM_TINT: Card = card("", "Purpurfilter im Vergrößerer: − grüner, + magenta. Wirkt nur beim Abzug auf Papier.", None);
static FILM_CONTRAST: Card = card("", "Steilheit der Filmkurven, wie längere oder kürzere Entwicklung: + härter, − weicher.", None);
static PAPER_CONTRAST: Card = card("", "Gradation des Papiers: + hart (kräftige Schwärzen, helle Lichter), − weich.", None);
static FILM_DIR: Card = card("", "DIR-Kuppler hemmen beim Entwickeln die Nachbarschichten. Das trennt Farben deutlicher und betont Kanten – wie bei modernen Farbfilmen. 0 = aus, 1 = wie der Film.", None);
pub static DR_CONTACT: Card = card(
    "Viele Fotos auf einem Blatt, oder ein Foto auf allen Papieren.",
    "## Kontaktabzug
Ein Blatt mit Filmstreifen zu je sechs Bildern, mit Perforation und Bildnummern, aus den ausgewählten Fotos (mindestens zwei), sonst dem aktiven und den folgenden. Alle Fotos bekommen Film und Papier des aktiven Fotos. „Exportieren …“ speichert es als JPEG oder TIFF.

## Alle Papiere
Jedes Feld zeigt dein Foto mit einem Papier (Zeile) und einem Grad (Spalte). Ein Klick übernimmt beides; der Bogen bleibt offen. Bei Diafilm zeigt die Reihe die Empfindlichkeiten (EI).

### Tasten
K öffnet den Kontaktabzug, K oder Esc schließt den Bogen.",
    None,
);
static FILM_GRAIN_SECTION: Card = card(
    "Filmkorn und Lichthof, physikalisch berechnet.",
    "Das Korn entsteht aus den Silberkörnern und Farbstoffwolken jeder Farbschicht: leicht farbig, in den Mitteltönen am stärksten. Wie stark es wirkt, hängt vom Format ab – siehe „Format“. Lichthof ist das rötliche Leuchten um helle Stellen.\n\n## Korngröße je Helligkeit\n• **Wie Film** – große Silberkristalle sind lichtempfindlicher. In dünnen Stellen des Negativs, den Schatten des Abzugs, entsteht darum gröberes Korn, in den Lichtern feineres. Deutlich bei klassischen Filmen (Tri-X, HP5), gering bei Tafelkristall-Filmen (Acros, T-Max, Delta), am stärksten bei Farbnegativ mit seinen schnellen und langsamen Teilschichten. An echten Filmscans gemessen.\n• **Gleichmäßig** – überall dieselbe Korngröße, das Modell aus spektrafilm.\n\nStandard ist „Wie Film“, bei Diafilm vorerst „Gleichmäßig“.\n\n### Tipp\n„Wie Film“ zeigt sich am deutlichsten bei Schwarzweiß mit Push-Entwicklung, in den Schatten bei 100 %.",
    None,
);
pub static FILM_SCAN: Card = card("Optionale Schwarz- und Weißpegel für den Direktscan.", "**Automatisch** nimmt 0,1 % und 99,9 % aus der ganzen Vorschau und speichert die Pegel; Ausschnitte und Export verwenden dieselben Werte. Flache Bilder werden nicht künstlich gespreizt.\n\nSchwarz 0 und Weiß 1 = unverändert. Bei Papier sind die Regler gesperrt.", None);
pub static FILM_CURVES: Card = card("Filmkurve, Papierkurve und Gesamtausgabe.", "• **Film**, **Papier** – normierte Datenblattdichte über der logarithmischen Belichtung\n• **Gesamt** – die tatsächlich berechnete neutrale Tonantwort mit Entwicklung, Grad, Push/Pull und Abzughelligkeit, ohne räumliche Effekte\n\nDie Y-Achsen haben unterschiedliche Bedeutung.", None);
pub static FILM_HUE: Card = card("Feinabstimmung der Grün- und Blautöne.", "0 ist der Standard des Films im gewählten Look.\n• **Grün** – dreht Gelbgrün und Grün um bis zu 8° Richtung Grün (links: Richtung Gelb)\n• **Blau** – dunkle Blautöne satter und bläulicher (links: blasser, türkiser)\n\nGrau, Haut, Rot und Orange bleiben unberührt. Bei „Abgestimmt“ bringt jeder Film schon eine kleine Korrektur mit, bei „Datenblatt“ gilt nur der Regler.", None);
pub static FILM_FOVEON: Card = card("Schwarzweißfilm direkt aus den drei Foveon-Schichten.", "Der Foveon-Sensor ist gebaut wie ein Film: drei Schichten übereinander, oben Blau, unten Rot.\n• **Aus den Sensorschichten belichten** – der Film bekommt seine Belichtung direkt aus den Schichten, gewichtet nach der gemessenen Empfindlichkeit von Sensor und Film. Rot, Blau und Laub werden so hell oder dunkel wie auf dem echten Film.\n• **Korn gleicht Sensorrauschen aus** – das Filmkorn wird um das Rauschen verringert, das schon im Foto ist. Ein Film wirkt bei ISO 100 und ISO 1600 gleich körnig. Bei Farbfilm wird zuerst das Farbrauschen stärker gemindert; die Farbwolken des Films treten an seine Stelle.\n\nNur für Sigma-Merrill-Fotos; aus den Schichten belichtet wird nur Schwarzweißfilm.\n\n### Tipp\nAufnahmefilter wirken wie vor dem Objektiv: Gelb, Orange und Rot dunkeln den Himmel ab.", None);
pub static FILM_LOOK: Card = card("Look des Films: Datenblatt oder abgestimmt.", "• **Datenblatt** – der Film, wie sein Datenblatt ihn beschreibt: ohne Standardfilter, Helligkeits-, Papier- und Gradvorgaben\n• **Abgestimmt** (Standard) – je Film ein nach Sicht abgestimmter Look: bei Schwarzweiß je Film ein passender Kontrastfilter, meist Gelb, bei Farbe etwas gepflegtere Grün- und satte dunkle Blautöne\n\nDie Regler unter „Farbton“ verstärken oder nehmen das zurück; danach ist alles frei einstellbar.", None);
pub static FILM_PRINT_COLOR: Card = card("Schwarzweißfilm auf Farbpapier.", "Wie im Farblabor: Das Schwarzweißnegativ wird auf Farbpapier belichtet; Gelb- und Magentafilter bestimmen den Ton.\n• **0** – neutral grau\n• **Warm / Kühl** – leicht braun / bläulich\n• **Magenta / Grün** – färben Mitteltöne und Lichter, die Tiefen bleiben dunkel\n\n### Tipp\nKleine Werte (0,2 bis 0,4) wirken wie ein echter Ton, große schnell künstlich.", None);
pub static FILM_HALO: Card = card("Lichthöfe und Streuung des Films.", "• **Film** – wie bisher\n• **Dezent, Stark, Rote Glut** – andere Kanalstärke und Ausbreitung\n• **Kein** – keine Streuung\n\nDie Regler für Lichthof und Radius bleiben wirksam.", None);
pub static FILM_DIFFUSION: Card = card("Lichtstreuung vor dem Objektiv oder beim Vergrößern.", "• **Weich, Nebel, Leuchten** – verteilen helle Bildteile unterschiedlich breit in ihre Umgebung; Haut und Kontrast wirken weicher\n\nBeim Direktscan wirkt nur der Filter vor dem Objektiv.\n\n### Tipp\nDetails bei 100 % beurteilen – die Vorschau hat einen kleineren Pixelmaßstab.", None);
pub static FILM_OPTICS: Card = card("Kamera, Vergrößerer und Scanner bilden gemeinsam die Schärfe-Kette.", "• **Standard** – wie bisher\n• **Scharf** – stärkere Scanner-Schärfung\n• **Weich, Alt** – Details optisch breiter verteilt\n\nNach Sicht abgestimmt, kein bestimmtes Objektiv. Einzelwerte überschreiben die Unschärfe der jeweiligen Stufe.", None);
pub static FILM_PREFLASH: Card = card("Zusätzliche gleichmäßige Belichtung des Fotopapiers.", "Schwärzt helle Papierstellen leicht und bringt dort Zeichnung zurück; der Abzug wird nicht heller. 0 = aus. Bei Farbpapier lässt sich das Vorlicht filtern.\n\n### Tipp\nKleine Werte genügen.", None);
pub static FILM_LOUPE: Card = card("Korn im gewählten Aufnahmeformat betrachten.", "Normal wird beim Zoomen nichts neu gerechnet, das Korn wirkt dann einfach vergrößert. Mit „Korn prüfen“ wird der Ausschnitt ab 200 % vor der Filmrechnung vergrößert; Korn und Farbstoffwolken entstehen neu in passender Filmgröße, jeder neue Ausschnitt braucht dann kurz. Mit lokalen Korrekturen bleibt die Pixelansicht.\n\n### Tasten\n⌘+ und ⌘− wechseln die Stufe.", None);
static FILM_GRAIN: Card = card("", "1 = wie der Film, 0 = kein Korn (nur Farben und Tonwerte des Films).\n\n### Tipp\nEingepasst oder im Abzugsformat beurteilen.", None);
static FILM_GRAIN_SIZE: Card = card("", "Größe der Kornklumpen: kleiner = feiner und ruhiger, größer = gröber.", None);
static FILM_HALATION: Card = card("", "Halation: Licht wird an der Filmrückseite zurückgeworfen und erzeugt rötliche Säume um helle Stellen (Lampen, Gegenlicht). 0 = aus, 1 = wie der Film.", None);
static FILM_HALATION_RADIUS: Card = card("", "Wie weit sich die Lichthöfe ausbreiten.", None);
// ── Library ──
static LIB_CATALOG: Card = card(
    "Die ganze Bibliothek und feste Auswahlen.",
    "• **Alle Fotos** – aus Lightroom-Import und hinzugefügten Ordnern\n• **Zuletzt importiert** – der letzte Lauf, der neue Bilder brachte\n• **Fehlende Fotos** – Datei nicht gefunden oder Volume nicht verbunden; gelöscht wird nichts\n\n**Auswahl** – Mit gedrückter Umschalttaste ziehen, um Fotos zur Auswahl hinzuzufügen. Ohne Umschalttaste lassen sich Fotos auf Sammlungen ziehen.",
    None,
);
static LIB_FOLDERS: Card = card(
    "Die Ordner auf der Platte, genau wie sie sind.",
    "Je Stammordner ein Baum mit Bildanzahl. graineo kopiert, verschiebt und benennt nichts um.\n\n## Grau, durchgestrichen\nSpeicherort nicht erreichbar. Die Fotos bleiben über die Vorschauen sichtbar; Sterne, Stichwörter und Personen lassen sich weiter ändern. Entwickeln, Export und Löschen brauchen das Original.\n\n## Ordner aufnehmen\n• **+** rechts im Kopf – nimmt einen Ordner in die Bibliothek auf; die Fotos bleiben, wo sie sind (wie „Fotos importieren …“ mit „Hinzufügen“)\n• **Ordner ansehen …** (⌘O, Menü Ablage) – nur ansehen, ohne Import; verändert nichts",
    None,
);
static LIB_COLLECTIONS: Card = card(
    "Sammlungen und Sammlungssätze aus Lightroom.",
    "Reihenfolge wie im importierten Katalog. Smart-Sammlungen (⚙) sind mit Namen übernommen; ihre Regeln wertet graineo noch nicht aus.",
    None,
);
static LIB_KEYWORDS: Card = card(
    "Stichwörter des Fotos.",
    "Das Feld fügt ein Stichwort hinzu (bei mehreren gewählten Fotos allen). Nach Stichwörtern filtern und sie verwalten (umbenennen, verschieben, löschen per Rechtsklick) geht im Metadaten-Filter der Bibliothek, Spalte „Stichwort“.",
    None,
);
static LIB_RATING: Card = card(
    "Sterne, Markierung und Farbetikett.",
    "Gespeichert in der Bibliothek, bei X3F zusätzlich in der Begleitdatei. Neben andere Originale wird nichts geschrieben.\n\n### Tasten\n0–5 Sterne · P markieren · X ablehnen · U aufheben · 6–9 Rot/Gelb/Grün/Blau (gleiche Taste nimmt das Etikett weg) · mit ⇧ danach zum nächsten Bild",
    None,
);
static LIB_METADATA: Card = card(
    "Angaben zur Datei.",
    "Aufnahmedaten aus dem Lightroom-Katalog bzw. der Datei selbst. „Zustand“ sagt, ob die Datei gerade erreichbar ist.",
    None,
);
static BIB_AI: Card = card(
    "KI-Beschreibung dieses Fotos.",
    "• **Szene und Begründung** – mit hervorgehobenen Suchtreffern\n• **Qualität** – gut, mittel, Ausschuss, dazu der Aussortier-Wert\n• **Ähnliche Fotos** – vergleicht die Bildinhalte, auch ohne gemeinsame Wörter\n\nEin Klick auf ein Stichwort sucht danach.",
    None,
);
pub static SENSOR_DEMOSAIC: Card = card(
    "Demosaik berechnet aus den Farbfilterwerten ein vollständiges Farbbild.",
    "RCD eignet sich für Bayer, Markesteijn für X-Trans. Markesteijn 3 ist genauer, Markesteijn 1 schneller; bilinear ist einfach und kann Farbsäume zeigen. Standard verwendet RCD für Bayer und Markesteijn 3 für X-Trans. Die neutrale Ansicht dient nur dem Vergleich und ändert die Entwicklung nicht. Unterschiede erkennt man bei 200–400 % an feinen kontrastreichen Strukturen, Gittern und Schrift; die Überlagerung Moiré hilft dabei.",
    None,
);
pub static SENSOR_BAR: Card = card(
    "Was der Sensor wirklich aufgenommen hat – vor Demosaik und Entwicklung.",
    "Alles wird hier im Fach „Sensor“ bedient; das Bild bleibt frei. Unten links im Bild steht, was gerade gezeigt wird.\n\n• **Überlagerungen** – legen Karten über das Bild, mehrere zugleich; „Deckkraft“ regelt alle, „Rauschkanal“ wählt R, G, B oder das Verhältnis zum Rauschmodell\n• **Darstellung** – Mosaik: die Rohwerte in ihrer Filterfarbe mit Pixelgitter, ab 400 %. Demosaik: ein anderes Verfahren für den sichtbaren Ausschnitt, ab 100 % (RCD für Bayer, Markesteijn für X-Trans, bilinear für beide)\n• **Am Zeiger** – Rohwerte unter dem Mauszeiger: DN und Filterfarbe des Pixels, Abstand zur Sättigung, Clipping, Rauschabstand und Kante\n• **RAW-Histogramm** – je Filterfarbe in Blenden unter der Sättigung, dazu die Reserve bis zur Sättigung\n• **Rauschabstand** – Linie: Rauschmodell der Kamera, Punkte: im Bild gemessen\n\n### Tasten\n• ⌥C Clipping · ⌥R JPEG-Reserve · ⌥F Schärfe · ⌥M Moiré\n• ⌥N Rauschen · ⌥D Defekte · ⌥L Objektiv\n• ⌥G Mosaik mit Pixelgitter\n• ⌥V / ⌥1–4 Demosaik: RCD, Markesteijn 3, Markesteijn 1, bilinear\n• J RAW-Clipping",
    None,
);
static SENSOR_CLIPPING: Card = card("Wo ein Farbkanal im RAW die Sättigung erreicht.", "• **Rot / Grün / Blau** – dieser Kanal geclippt\n• **Weiß** – alle Kanäle\n\nGemessen an der Häufung am Höchstwert (echtes Clipping), nicht am nominellen Weißpunkt. Markiertes holt auch „Lichter“ nicht zurück.", None);
static SENSOR_RESERVE: Card = card("Ausgefressen im Kamera-JPEG – aber noch nicht im RAW?", "Vergleicht das eingebettete Kamera-JPEG mit den Rohdaten:\n• **Grün** – im JPEG weiß, im RAW noch mindestens ½ Blende Reserve (rettbar)\n• **Gelb** – weniger als ½ Blende\n• **Rot** – auch im RAW geclippt", None);
static SENSOR_SHARPNESS: Card = card("Fokus-Peaking aus den Grünpixeln, vor dem Demosaik.", "Markiert die stärksten Kanten (Tenengrad auf der Grünebene, Rauschen abgezogen). So sieht man die Schärfeebene, ohne dass Nachschärfen der Entwicklung mitspielt.", None);
static SENSOR_MOIRE: Card = card("Wo Falschfarben und Moiré drohen.", "Hochfrequente Farbanteile nach dem Referenz-Demosaik (RCD bzw. Markesteijn 3). Magenta = Gefahr.\n\n### Tipp\nZum Vergleichen der Verfahren am Ort vergrößern und unter „Darstellung“ ein Demosaik wählen.", None);
static SENSOR_NOISE: Card = card("Rauschabstand je Kanal bzw. gemessen gegen Modell.", "• **R / G / B** – Rauschabstand in flachen Blöcken (0–45 dB)\n• **Verhältnis** – gemessenes Rauschen durch das Rauschmodell der Kamera: blau weniger, weiß wie erwartet, rot mehr (Textur, Banding, Bearbeitung in der Kamera)", None);
static SENSOR_DEFECTS: Card = card("Hot-, Dead- und Stuck-Pixel.", "Gefunden im Vergleich mit gleichfarbigen Nachbarn. Wenige Hot-Pixel bei langen Belichtungen sind normal; die Entwicklung ersetzt sie.", None);
static SENSOR_LENS: Card = card("Vignettierung und Verzeichnung des Objektivs.", "• **Orange** – Helligkeitsabfall laut Profil (Kamera bzw. lensfun)\n• **Grünes Gitter** – Verzeichnung, wie die Korrektur sie aufheben würde", None);
static SENSOR_MOSAIC: Card = card("Die Rohwerte in ihrer Filterfarbe, Pixel für Pixel.", "Ab 400 % zeigt der sichtbare Ausschnitt das Farbfilter-Mosaik (Bayer bzw. X-Trans) mit Pixelgitter – so sieht der Sensor das Bild vor jedem Demosaik. Beim Überfahren eines Pixels erscheinen dessen Rohwert und gegebenenfalls der Defekttyp am Zeiger. Ein Fadenkreuz und eine Markierung zeigen den gewählten Pixel.", None);

/// Card of a sensor overlay (by shortcut ID).
pub fn sensor_card(id: &str) -> &'static Card {
    match id {
        "sensor_clipping" => &SENSOR_CLIPPING,
        "sensor_reserve" => &SENSOR_RESERVE,
        "sensor_sharpness" => &SENSOR_SHARPNESS,
        "sensor_moire" => &SENSOR_MOIRE,
        "sensor_noise" => &SENSOR_NOISE,
        "sensor_defects" => &SENSOR_DEFECTS,
        "sensor_lens" => &SENSOR_LENS,
        _ => &SENSOR_MOSAIC,
    }
}
pub static LIBRARY_CHECK: Card = card(
    "Bibliothek mit den Ordnern abgleichen.",
    "• **Prüfen** – gleicht die Bibliothek mit den Ordnern ab: nimmt neue Fotos auf, meldet fehlende und nicht verbundene\n• **Neu aufbauen** – erzeugt die Bibliothek frisch aus Ordnern, Begleitdateien und Lightroom; die alte bleibt daneben liegen\n\nOhne Begleitdateien stecken die Bearbeitungen nur in der Bibliothek und ihren Sicherungen.",
    None,
);
pub static PROOF: Card = card(
    "Zeigt das Bild so, wie es in einem kleineren Farbraum aussähe – nur die Anzeige, Bearbeitung und Export bleiben gleich.",
    "• **Farbproof** – Ansicht → Farbproof: Voller Umfang, sRGB, Display P3, Adobe RGB oder ProPhoto. Farben außerhalb des gewählten Raums werden auf seinen Rand gesetzt. ⌘Y wechselt zwischen vollem Umfang und dem zuletzt gewählten Raum.\n• **Farbumfang-Warnung** – ⇧⌘Y färbt Bildpunkte magenta, die außerhalb des Proof-Raums liegen.\n\nGilt überall, wo Entwickeln das Foto zeigt: eingepasst, vergrößert, in der Lupe, im Navigator und beim Vergleichen, auch in der Dunkelkammer. Miniaturen (Bibliothek, Filmstreifen) sowie Film und Schwarzweiß sind sRGB-Bilder; sie liegen in jedem Proof-Raum, dort ändert der Proof nichts. Die Wahl gilt bis zum Beenden; hier stellst du ein, womit graineo startet.",
    None,
);

pub static HISTORY: Card = card(
    "Jeder Schritt der Bearbeitung dieses Fotos, der neueste oben.",
    "• **Klick** auf einen Schritt springt dorthin; das Bild zeigt seinen Stand. Es entsteht kein neuer Schritt\n• **▶** markiert den Schritt, der gerade gilt\n• **Gedimmte Schritte** liegen nach dem Zeiger und bleiben anklickbar. Die nächste Änderung verwirft sie; ⌘Z gleich danach holt sie zurück\n• **Überfahren** zeigt den Stand eines Schritts im Bild, ohne zu springen\n• **Original** springt zum Ausgangszustand, so wie graineo das Foto zuerst entwickelt hat\n• **(3×)** – Änderungen am selben Regler kurz hintereinander, zusammengefasst\n\nDer Verlauf steht in der Bibliothek auf diesem Rechner (Einstellungen → Bibliothek: „Verlauf speichern“).",
    None,
);
pub static SIDECARS: Card = card(
    "Bearbeitungen, Sterne und Stichwörter zusätzlich als XMP-Datei neben jedes Foto schreiben.",
    "graineo speichert alles in der Bibliothek. Mit diesem Schalter steht zusätzlich eine Kopie als XMP neben dem Foto – für andere Programme, und sie wandert mit dem Foto mit. Die Originale bleiben unverändert.\n• **Begleitdateien einlesen** – holt Änderungen anderer Programme aus diesen Dateien in die Bibliothek",
    None,
);
pub static METADATA_REREAD: Card = card(
    "Liest die Aufnahmedaten der gewählten Fotos neu aus den Dateien.",
    "• **Was die Datei trägt, gewinnt** – Datum, Kamera, Objektiv, Belichtung, Ort, Maße\n• **Was fehlt, bleibt** – z. B. ein Objektiv aus Lightroom; eine korrigierte Aufnahmezeit bleibt immer\n• **Aus XMP** (nur wenn eine Begleitdatei daneben liegt und du es einschaltest) – Bewertung, Etikett und Stichwörter ersetzen die Werte der Bibliothek\n\nNie geändert: Entwicklung, virtuelle Kopien, Sammlungen und die Dateien selbst. ⌘Z nimmt den ganzen Lauf zurück, auch dabei neu angelegte Stichwörter.",
    None,
);
// The camera import (USB/PTP, `import_camera.rs`) is prepared but not switched on yet: the card does not mention it.
pub static IMPORT: Card = card(
    "Bringt Fotos von einer Karte oder aus einem Ordner in die Bibliothek.",
    "• **Hinzufügen** – die Fotos bleiben, wo sie sind; ihre Ordner kommen in die Bibliothek (abgehakt wird je Ordner)\n• **Kopieren** – jede Datei wird ins Ziel kopiert, danach vom Ziel zurückgelesen (am Zwischenspeicher des Macs vorbei) und mit SHA-256 verglichen; erst dann gilt sie als importiert. Die Quelle bleibt unverändert\n• **Verschieben** – wie Kopieren; die Originale eines Fotos kommen erst in den Papierkorb, wenn alle seine Kopien (RAW, JPEG, XMP, zweite Kopie) geprüft sind\n\n### Marken\n• **schon da** – dieselbe Datei (gleicher Inhalt, Name egal) ist schon in der Bibliothek; sie wird übersprungen\n• **vermutlich da** – gleiche Größe und gleicher Name oder Aufnahmezeit wie ein Foto, dessen Laufwerk gerade nicht verbunden ist; nicht angehakt, du entscheidest\n• **identisch** – bytegleiche Kopie eines anderen Fotos in derselben Quelle; nur das erste wird importiert\n\n### Im Hintergrund weiter\nSchließt das Fenster, der Import läuft weiter und graineo bleibt bedienbar. Die Statusanzeige oben zeigt den Fortschritt, ein Klick darauf holt das Fenster zurück. Fehlt unterwegs das Ziel oder die Karte, wartet der Import, bis sie wieder da sind.",
    None,
);
pub static LR_IMPORT: Card = card(
    "Holt Fotos, Bewertungen, Stichwörter, Sammlungen, Gesichter und Bearbeitungen aus dem Lightroom-Katalog in die Bibliothek.",
    "Der Katalog wird nur gelesen; Lightroom und die Fotos bleiben unverändert.\n• **Prüfen** – liest den Katalog und zeigt, was kommt; geschrieben wird noch nichts\n• **Importieren** – schreibt in die Bibliothek; die Bearbeitungen kommen im selben Lauf mit\n• **Erneut prüfen** – holt spätere Änderungen aus Lightroom nach; was du seither in graineo geändert hast, bleibt\n• **Sammlungssatz** – die Sammlungen des Katalogs kommen in diesen Satz; leer: ganz oben\n• **Smart-Sammlungen** – kommen nur mit ihrem Namen\n\n### Tipp\nLightroom vorher beenden, sonst fehlen die neuesten Änderungen.",
    None,
);
pub static LR_HANDOVER: Card = card(
    "Übernimmt die Lightroom-Bearbeitungen in die Bibliothek.",
    "Übernommen werden Entwicklung, Freistellen und Drehung, Masken und virtuelle Kopien. Danach braucht graineo den Katalog nicht mehr.\n• Eigene Einstellungen aus graineo bleiben; schon übernommene Fotos werden übersprungen.\n• **Abbrechen** hält nach dem aktuellen Foto an; ein neuer Lauf macht dort weiter.\n• Fotos auf nicht verbundenen Laufwerken folgen beim nächsten Lauf.\n\n### Tipp\nDas Protokoll liegt im Ordner „reports“ der Bibliothek.",
    None,
);

pub static UPDATES: Card = card(
    "graineo fragt einmal am Tag bei GitHub nach einer neuen Version.",
    "Geladen wird nur eine kleine Datei mit der neuesten Versionsnummer; über dich, deine Fotos oder deinen Rechner wird nichts gesendet.\n• **Installieren** – lädt das Paket, prüft Signatur und Prüfsumme, ersetzt die App und startet sie neu; die alte kommt in den Papierkorb\n• **Schalter aus** – graineo sucht nur, wenn du es auslöst",
    None,
);

/// "RAW+JPEG doppelt": the same shot twice in the library.
pub static RAW_JPEG_PAIRS: Card = card(
    "Dasselbe Foto liegt zweimal in der Bibliothek: als RAW und als Kamera-JPEG.",
    "Ein Paar sind zwei Fotos mit gleicher Aufnahmezeit (± 1 s) und gleicher Kamera.\n• **Sicher** – zusätzlich gleicher Dateiname und passende Größe; das RAW ist erreichbar. Diese sind vorab angehakt.\n• **Wahrscheinlich** – anderer Name oder abweichende Größe; nur auf Wunsch entfernen. Serienbilder landen nie hier.\n• **RAW nicht erreichbar** – Laufwerk nicht verbunden oder Datei fehlt; das JPEG bleibt.\n• **Hat eigene Daten** – das JPEG ist bearbeitet oder hat virtuelle Kopien; es bleibt.\nEntfernt wird immer nur das JPEG, standardmäßig in den Papierkorb. Bewertung, Markierung, Etikett, Stichwörter, Personen und Sammlungen, die das RAW nicht hat, wandern vorher aufs RAW. Offline-Kopien (kleine Kopien von RAWs auf dem NAS) sind keine Fotos der Bibliothek und werden nie berührt.",
    None,
);

/// Library: "Nicht in der Bibliothek".
pub static NOT_IN_LIBRARY: Card = card(
    "Ordner in deinen Stammordnern, deren Fotos die Bibliothek noch nicht kennt.",
    "graineo durchsucht die Stammordner im Hintergrund (nur lesend) und listet Ordner mit Fotos, die nicht in der Bibliothek sind.\n\n### Warum fehlen sie\n• **Lightroom kannte diese Ordner nicht** – in Stammordnern aus dem Lightroom-Import nimmt der Abgleich neue Fotos in bekannten Ordnern auf, aber nie ganze neue Ordner (nie importiert, in Lightroom entfernt oder später auf der Platte angelegt).\n• **Neu in eigenen Stammordnern** – dort nimmt der nächste Abgleich den Ordner von selbst auf.\n\nOrdner, die du aus der Bibliothek entfernt hast, erscheinen hier nicht wieder.\n\n### Was tun\n• **Zur Bibliothek hinzufügen** – legt Bibliothekseinträge an und liest Bewertungen, Stichwörter und Bearbeitungen aus vorhandenen XMP-Dateien. Die Fotos selbst bleiben unverändert. Fotos, die schon an anderer Stelle in der Bibliothek liegen, erscheinen danach unter „Duplikate“.\n• **Nicht mehr anzeigen** – nur ausblenden; „Ausgeblendete wieder zeigen“ holt sie zurück.\n• **Doppelklick** – den Ordner nur ansehen.",
    None,
);

/// Library: "Duplikate" (group view).
pub static DUPLICATES: Card = card(
    "Fotos, die mehrfach in der Bibliothek liegen, in Gruppen nebeneinander – mit Vorschlag, welches bleibt.",
    "• **Gleich** – gleiche Größe, Aufnahmezeit und Prüfsumme (Anfang und Ende der Datei). Vor dem Entfernen wird der ganze Inhalt verglichen (SHA-256). Nur hier ist der Vorschlag schon markiert.\n• **Ähnlich** – Bildinhalt fast gleich, am selben Aufnahmetag (Serien, Exporte neben dem Original). Nie automatisch markiert.\n\n### Welches bleibt\nDer Reihe nach entscheidet: erreichbar · eigene Entwicklung oder virtuelle Kopien · Bewertung, Stichwörter, Sammlungen, Personen · RAW vor JPEG · höhere Auflösung · größere Datei · Hauptordner vor Ordnern wie „Backup“, „Kopie“, „Export“ · älter. Das bleibende steht vorn, mit seinem Vorteil.\n\n### Bedienen\nKlick markiert ein Foto zum Entfernen oder behält es wieder; Doppelklick behält nur dieses. Je Gruppe bleibt immer mindestens ein Foto.\n\n### Sicherheit\nNur in den Papierkorb. Bewertung, Markierung, Etikett, Stichwörter, Personen und Sammlungen wandern vorher aufs bleibende Foto (nur ergänzt). Fotos aus Apple Fotos, mit eigener Entwicklung oder nicht erreichbare bleiben immer. ⌘Z holt die Bibliothekseinträge zurück.",
    None,
);

/// "Ordner ansehen" (⌘O): the "?" in the band (view_folder_band.rs). Says only what the code does: develop edits are
/// saved as a sidecar next to the photo (`raw_app::save_to_sidecar`), ratings only with the switch, filters on stars,
/// flags and labels work, the search finds file names only (the rows of a viewed folder have no camera, date or
/// keywords), everything kept in the library comes only after adding.
pub static VIEW_FOLDER: Card = card(
    "Der Ordner ist nur geöffnet, nicht in der Bibliothek. Die Fotos selbst verändert graineo nie.",
    "### Was gespeichert wird\n• **Entwickeln** – jede Änderung an den Reglern (auch virtuelle Kopien) wird als Begleitdatei NAME.xmp neben dem Foto gespeichert\n• **Bewerten** (Sterne, Markierung, Etikett) – gesperrt, solange „Bewertungen als XMP“ aus ist; eingeschaltet landen sie ebenfalls als XMP neben den Fotos\n\n### Was hier geht\n• Ansehen, Entwickeln, Exportieren\n• Filtern nach Sternen, Markierung und Etikett (Werte aus vorhandenen XMP)\n• Suchen nach Dateinamen\n\n### Erst nach dem Hinzufügen\nSpaltenfilter (Kamera, Objektiv, Datum …), Suche nach Inhalt, Stichwörtern und Orten, Sammlungen, Personen, Duplikate und Serien. Beim Hinzufügen übernimmt die Bibliothek Bewertungen und Entwicklungen aus den XMP.\n\n### Ordnerbaum\nLinks unter „Angesehen“ zeigt ein Klick auf einen Ordner seine Fotos; „Unterordner einbeziehen“ aus: nur die Fotos direkt darin. **Aktualisieren** liest den Ordner neu, leise auch, wenn du zu graineo zurückkehrst.\n\n### Verlassen\n**Schließen** im Streifen oder per Rechtsklick auf den Ordner links. Eine andere Quelle zeigt die Bibliothek; der Ordner bleibt unter „Angesehen“. **Zur Bibliothek hinzufügen …** öffnet den Import mit diesem Ordner.",
    None,
);

/// Search field of the library: syntax, typed suggestions and the fields with all their names.
pub static SEARCH: Card = Card {
    short: "Freie Wörter und Feldbegriffe, kombinierbar.",
    text: "Freie Wörter suchen in Dateiname, Ordner, Stichwörtern, Personen, KI-Beschreibung, Bildtext und Sammlungen, auch Wortteile und Wortformen.\n\n### Vorschläge\nBeim Tippen zeigt die Liste passende Personen, Orte, Stichwörter, Sammlungen, Ordner, Kameras und Objektive mit ihrer Art. „:“ oder der Anfang eines Feldnamens zeigt die Felder; Tab oder ↩ übernimmt, z. B. camera:\"\" mit dem Cursor zwischen den Anführungszeichen, danach folgen die Werte. ↓/↑ wählen, Esc schließt die Liste.\n\n### Suchsyntax\na | b – eines · -wort – ohne · \"…\" – genau so\naperture:<=2.8 focal:20-35 iso:>=1600 shutter:<=1/60\nyear:2020 month:7 day:15 weekday:sa time:abend date:>2020-05\nrating:>=3 label:red flag:picked ai:good has:person no:keyword\n\nOrte sind Stichwörter unter einem Oberbegriff „Orte“ (oder „Places“).",
    example: None,
    extra: Some(crate::library_ui::search_field_table),
};

/// German text of the search card (its test checks every example in every language).
#[cfg(test)]
pub(crate) fn search_card_text() -> &'static str {
    SEARCH.text
}

/// Folder context menu: "Move to …"
pub static MOVE_FOLDER: Card = card(
    "Einen Ordner an einen anderen Ort bringen, auch auf ein anderes Laufwerk, auch in einen vorhandenen Ordner.",
    "Ordner für Ordner: Kopie mit Prüfsumme, Bibliothek umstellen, erst dann gehen die alten Dateien in den Papierkorb. Auf demselben Laufwerk wird nur umbenannt.\n• **Gleiche Datei am Ziel** – gilt als vorhanden\n• **Anderer Inhalt, gleicher Name** – nie überschrieben; bleibt liegen und steht im Bericht\n• **Inhalt direkt einsortieren** – Inhalt statt Ordner ins Ziel\n\n### Tipp\nAnhalten geht jederzeit, auch nach Absturz geht es weiter. Unter 20 GB frei hält es an: Papierkorb leeren, fortsetzen.",
    None,
);

/// Folder context menu: "Locate missing folder …"
pub static RELINK_FOLDER: Card = card(
    "Einen im Finder verschobenen oder umbenannten Ordner wieder verknüpfen.",
    "Zeige graineo, wo der Ordner jetzt liegt. Verglichen werden Name, Größe und Änderungszeit der Fotos, auch in Unterordnern.\n• **Passt nicht alles** – der Dialog nennt die Zahl und fragt nach\n• **Passt nichts** – es wird nichts verknüpft\n\nDie Dateien bleiben unberührt; Bewertungen, Stichwörter, Entwicklung und Vorschauen bleiben erhalten.\n\n### Tipp\nIst nur das Laufwerk getrennt, genügt es, es zu verbinden.",
    None,
);

/// Export dialog: what "lossless" means for Foveon data (three levels).
pub static EXPORT_LOSSLESS: Card = card(
    "„Verlustfrei“ heißt bei Foveon-Dateien drei verschiedene Dinge.",
    "• **Sensorschichten** – TIFF „Sensorschichten“: die drei Schichten unverändert wie in der X3F, ohne Farbe; Schwarzwert und Matrix stehen in der Datei\n• **Linear, nichts beschnitten** – Lineares DNG, TIFF 32 Bit, OpenEXR: ohne Tonkurve; Lichter über Weiß und Farben außerhalb des Farbraums bleiben erhalten\n• **Fertiges Bild** – TIFF oder PNG 16 Bit, JPEG XL verlustfrei: Tonkurve eingerechnet, für Druck und Retusche\n\nAlles davon hält nur die X3F selbst – Kalibrierung und Dunkelrahmen gibt es nur dort.\n\n### Wohin\nLightroom und Camera Raw: Lineares DNG. Resolve, Nuke, Affinity: OpenEXR. Die macOS-Vorschau öffnet DNG mit 32 Bit nicht.",
    None,
);


/// Settings → Library → Previews
pub static PREVIEWS: Card = card(
    "Vorschauen dienen nur der Anzeige, nie dem Export.",
    "• **Kleine Vorschau** – für Raster, Filmstreifen und Suche; entsteht im Hintergrund für alle Fotos. 1024 px bleibt auch groß scharf, 512 px braucht ein Viertel des Platzes.\n• **Offline-Kopie** – nur für Fotos auf anderen Laufwerken (NAS, externe Platten); zeigt das Foto auch getrennt und wird nie gelöscht.\n\nUnter 20 GB freiem Platz wird nichts mehr geschrieben. Vorschauen lassen sich jederzeit neu erzeugen.",
    None,
);

/// Settings → Library → Cache limit
pub static CACHE_LIMIT: Card = card(
    "Begrenzt den Platz, den der Cache auf diesem Mac belegen darf.",
    "• **Unbegrenzt** (Standard) – es wird nie etwas automatisch entfernt.\n• **10 bis 100 GB** – ist der Cache größer, entfernt graineo im Hintergrund, sobald der Mac nichts anderes tut, die ältesten Foveon- und HDR-Zwischenspeicher und die am längsten nicht angesehenen Vorschauen von Fotos auf diesem Mac, bis etwa 90 % der Grenze erreicht sind.\n\nGezählt wird alles im Cache. Nie entfernt werden Offline-Kopien und Vorschauen von Fotos auf anderen Laufwerken, entwickelte Vorschauen, Fingerabdrücke und Gesichter; die zuletzt benutzten Vorschauen bleiben ebenfalls. Originale, Bearbeitungen und die Bibliothek liegen nicht im Cache und sind nie betroffen.\n\n### Hinweis\nEntfernte Vorschauen entstehen bei Bedarf neu. Das kostet beim Blättern Rechenzeit, besonders bei Sigma Quattro und Fotos auf dem NAS.",
    None,
);

// ── Non-Sigma RAWs (crates/rawdev) ──
pub static RAW_LR_START: Card = card(
    "Die Regler starten mit den Werten, die dieses Foto in Lightroom hatte.",
    "Gelesen werden alle Grundregler, Weißabgleich, Kurve, Farbmischer, Schärfen, Rauschminderung, Objektivprofil, Freistellen und das Profil (z. B. „Camera Classic Chrome“ → Kamera-Look). Der Katalog wird nur gelesen.\n\nSehr ähnlich, aber nicht pixelgleich – die Rechenkette ist eine andere. „Zurücksetzen“ beginnt mit der eigenen Standardentwicklung.",
    None,
);
static RAW_TEMPERATURE: Card = card("", "Die Kelvin-Angabe gilt für die Lichtquelle: Ein Glühlampenfoto wird mit etwa 2 850 K neutral. Umgerechnet wird über die Farbmatrizen der Kamera (DNG-Verfahren); die Helligkeit bleibt gleich.", None);
static RAW_TINT: Card = card("", "Verschiebt quer zur Farbtemperatur: + Richtung Magenta, − Richtung Grün. Hilft bei Leuchtstofflicht oder Laub.", None);
static RAW_EXPOSURE: Card = card("", "Wirkt wie eine längere oder kürzere Belichtung auf den linearen Sensordaten, vor der Tonkurve. Ausgefressene Lichter werden vorher rekonstruiert.", None);
static RAW_CONTRAST: Card = card("", "Die Tonkurve ist ein weiches S (Sigmoid). + macht sie in der Mitte steiler, − flacher; Schwarz und Weiß bleiben dabei an ihrem Platz.", None);
static RAW_HIGHLIGHTS: Card = card("", "Wirkt örtlich: Eine weiche Helligkeitskarte zeigt helle Bildbereiche, dort wird um bis zu 1,5 Blenden abgedunkelt (−) bzw. aufgehellt (+). Himmel und Wolken bekommen so Zeichnung, ohne dass das ganze Bild flach wird.", None);
static RAW_SHADOWS: Card = card("", "Wirkt örtlich in dunklen Bildbereichen, um bis zu 1,6 Blenden. Ideal bei Gegenlicht.", None);
static RAW_WHITES: Card = card("", "Verschiebt nur die hellsten Töne (je Pixel, nicht örtlich): + lässt Lichter früher weiß werden, − holt sie zurück.", None);
static RAW_BLACK: Card = card("", "Verschiebt nur die tiefsten Töne: − macht Schwarz satter, + hebt es an.", None);
static RAW_TEXTURE: Card = card("", "Verstärkt oder glättet Details von wenigen Pixeln auf der Helligkeit – zwischen Schärfen und Klarheit.\n\n### Tipp\nBei 100 % beurteilen.", None);
static AI_DENOISE: Card = card(
    "Entrauscht das RAW mit einem neuronalen Netz, einmal in voller Auflösung.",
    "Ein kleines Netz (RawNIND, GPL-3.0), trainiert auf echten Rauschpaaren, rechnet nach dem Entmosaiken auf den linearen Daten. Es erfindet keine Motive; bei sehr starkem Rauschen können glatte Flächen leicht fleckig wirken, dann die Stärke senken. Die Rauschregler darunter wirken auf das Restrauschen. Dauer auf einem M1 Pro etwa 18 s je 24 MP; das Ergebnis bleibt, solange das Foto offen ist. Das Modell lädt man in Einstellungen → Module.",
    None,
);
static RAW_NR: Card = card("", "Rauschminderung auf dem demosaikierten Bild mit einem aus dem Bild geschätzten Rauschmodell, getrennt nach Helligkeit und Farbe. Detail schont feine Strukturen. Standard: Farbe 25, Luminanz 0.", None);
/// Test shots for a new lens (assistant "Objektiv hinzufügen").
pub static LENS_SHOTS: Card = card("", "• **Gitter** (Verzeichnung, Farbsäume): ein Gitter oder gerade Linien über das ganze Bild bis in die Ecken, Kamera parallel zur Fläche, Stativ, Blende f/8. Mehrere Aufnahmen derselben Brennweite zählen zusammen.\n• **Weißfeld** (Vignettierung, Farbstich): eine gleichmäßig helle Fläche, zum Beispiel weißes Papier oder ein Diffusor vor dem Objektiv, bewusst unscharf, nichts ausgebrannt. Je Blende und Brennweite eine Aufnahme.\n• **Paar:** danach dieselbe Aufnahme mit um 180° gedrehter Kamera. So hebt sich ein Helligkeitsverlauf der Beleuchtung auf. Ohne Paar bleibt die Güte bei höchstens 70.\n• **Immer:** RAW, ISO 100, Autofokus und Bildstabilisator aus, die Objektivkorrektur der Kamera aus.\n\n### Sigma-X3F\nDer Farbstich wird je Sensorschicht gemessen (obere, mittlere, untere Schicht).", None);
static RAW_LENS: Card = card("", "Korrigiert Verzeichnung, Farbquerfehler und Vignettierung. Quelle der Reihe nach:\n• eigenes Profil\n• Adobe-Profil aus dem DNG Converter (lokal gelesen)\n• Daten der Kamera in der Datei\n• lensfun\n\nDas Etikett nennt die verwendete Quelle.", None);

pub static SENSOR_VIEWS: Card = card(
    "Zeigt, was der Sensor in jeder Schicht aufgenommen hat.",
    "• **Entwickeltes Bild** – normale Entwicklung mit allen Reglern\n• **Obere Schicht** – fast panchromatisch, rauscht am wenigsten; Basis für Top-Layer-Monochrom\n• **Mittlere Schicht** – Schwerpunkt Grün\n• **Untere Schicht** – Schwerpunkt Rot und Infrarot; meistes Rauschen, größte Diffusionsunschärfe\n• **Alle drei roh als R/G/B** – unten Rot, Mitte Grün, oben Blau, ohne Farbmatrix: so „sieht“ der Sensor\n• **Farbanteil der Schichten** – jede Schicht durch die hellste geteilt, ohne Helligkeit\n• **Raw-Clipping je Schicht** – gesättigte Stellen: rot = unten, grün = Mitte, blau = oben, Mischfarben = mehrere\n\n### Tipp\n„Helligkeit“ ändert nur die Anzeige. Bei beschädigten Dateien zeigt „Gerettete Bereiche zeigen“ geschätzte Zeilen gelb und grau gefüllte schraffiert.",
    None,
);
pub static SIGMA_DETAIL: Card = card(
    "Entrauscht und schärft in Grund- und Detailebene.",
    "Zerlegt das Bild in Grund- und Detailebene, entrauscht und schärft dabei – das ergibt den typischen Foveon-Eindruck, abgestimmt auf das Aussehen von SIGMA Photo Pro in der Grundeinstellung. Ausgeschaltet wird das Bild deutlich weicher. Luminanz und Farbe regeln das Rauschen.\n\nDie Regler **Feindetail** und **Struktur** ersetzen diesen Schalter (Merrill, Quattro, Classic).",
    None,
);
pub static DETAIL_FINE: Card = card(
    "Wie viel der feinsten Foveon-Detailebene zurückkommt.",
    "Sigmas Entrauschen trennt ein glattes Grundbild von der Detailebene und setzt das Detail danach wieder ein. graineo teilt diese Detailebene in zwei Bänder:\n• **Feindetail** – kleinste Strukturen bis etwa 3 Pixel: Poren, Hauttextur, Restrauschen\n• **Struktur** – gröbere Strukturen bis etwa 30 Pixel: Wimpern, Haare, Augen, Kanten\n\n**100** = Sigmas Detail unverändert (wie SIGMA Photo Pro). Weniger Feindetail glättet Haut, ohne Augen und Haare weich zu machen; mehr betont feinste Textur samt Rauschen. Das Rauschen selbst regeln weiter Luminanz und Farbe.\n\n### Tipp\nFür Porträts: Feindetail niedrig, Struktur etwas höher (Vorgabe „Porträt (natürlich)“). Bei 100 % beurteilen.",
    None,
);
pub static DETAIL_STRUCTURE: Card = card(
    "Wie viel der gröberen Foveon-Detailebene zurückkommt.",
    "Das gröbere der beiden Detailbänder (etwa 3 bis 30 Pixel): Wimpern, Haare, Augen, Kanten und das Relief der Haut. **100** = Sigmas Detail unverändert; weniger wirkt weicher und flacher, mehr plastischer. Großflächige Helligkeitsverläufe bleiben immer unverändert. Gegenstück: **Feindetail**.",
    None,
);
pub static BINNING: Card = card(
    "Mittelt bei hoher ISO benachbarte Pixel zu einem – weniger Rauschen, kleineres Bild.",
    "Nach Sigmas Stufen bei sd Quattro und sd Quattro H. Je Kante:\n• **ab ISO 800** ×2 – ¼ der Pixel (sd Quattro 2712 × 1808)\n• **ab ISO 1600** ×3 – ⅑ der Pixel (1808 × 1205)\n• **ab ISO 6400** ×4 – 1/16 der Pixel (1356 × 904)\n\n• **Auto** – nach Sigmas Stufen\n• **Aus** – volle Größe, mehr Rauschen\n• **An · graineo** (dp Quattro) – dieselben Stufen; Sigma selbst wendet beim dp Quattro kein Binning an\n\nUnter ISO 800 ohne Wirkung. Ansicht, 100 %, Export und Größenbegrenzung richten sich nach der kleinen Größe; Zuschnitt, Masken und Retusche bleiben an ihrer Stelle. Vorher/Nachher kann links dasselbe Foto mit der anderen Einstellung zeigen. Standard für neue Fotos: Einstellungen → „Entwickeln“.",
    None,
);
pub static NOISE_BRAKE: Card = card(
    "Wie stark Rauschen beim Aufhellen mit Fill Light mitwächst.",
    "Fill Light hebt dunkle Bildteile samt Rauschen an.\n• **Aus** – Rauschen wächst mit\n• **Farbe** – Farbrauschen (bunte Flecken) wächst nicht mit; Schärfe und Korn bleiben\n• **Farbe + Korn** – dämpft zusätzlich das Helligkeitskorn; dort etwas weicher\n\n### Tipp\nWirkt erst bei 100 % und im Export (etwa 1 s); die Vorschau bleibt schnell.",
    None,
);
pub static NOISE_BRAKE_RAW: Card = card(
    "Wie stark Rauschen beim Aufhellen mitwächst.",
    "Belichtung, Tiefen und Weiß heben dunkle Bildteile samt Rauschen an.\n• **Aus** – Rauschen wächst mit\n• **Farbe** – Farbrauschen wächst nicht mit; Schärfe und Korn bleiben\n• **Farbe + Korn** – dämpft zusätzlich das Helligkeitskorn; dort etwas weicher\n\n### Tipp\nDer Export dauert damit etwa doppelt so lange; die Vorschau bleibt schnell.",
    None,
);
static DEHAZE: Card = card(
    "Nimmt Dunst und Schleier aus dem Bild.",
    "Schätzt Farbe und Dichte des Dunstes je Bildstelle („Dark Channel“). Ferne, dunstige Bereiche gewinnen Kontrast und Farbe, nahe bleiben fast gleich.\n• **+** – entfernt Dunst; das Bild wird etwas dunkler (zur Hälfte ausgeglichen)\n• **−** – legt einen Schleier in der Dunstfarbe darüber\n\n### Tipp\nHohe Werte verstärken Rauschen in dunklen Flächen. Bei Nicht-Sigma-Fotos dauert der Export etwa doppelt so lange.",
    None,
);
static TONE_EQ: Card = card(
    "Ein Regler für Schatten und Lichter, mit Schutz der jeweils anderen Seite.",
    "• **Ausgleich nach rechts** – hellt Schatten und Mitteltöne auf. Lichter ab einer Blende unter Weiß bleiben garantiert unverändert, nichts wird neu ausgefressen.\n• **Ausgleich nach links** – dunkelt Lichter und Mitteltöne ab. Die tiefen Schatten bleiben garantiert unverändert.\n\nEine Maske, die Flächen glättet und an Kanten stehen bleibt, sorgt dafür, dass ganze Flächen (ein Gesicht im Gegenlicht) gemeinsam heller werden – ohne Lichthöfe an den Kanten.\n### Feineinstellung\nNeun Zonenregler von −8 EV (fast Schwarz) bis 0 EV (Weiß) kommen zum Ausgleich hinzu; dazu Glättung, Kantenschärfe und „Maske zeigen“. Gesetzte Zonen können den Schutz aufheben.",
    None,
);
static TONE_EQ_ZONE: Card = card(
    "Hebt (+) oder senkt (−) die Belichtung dieser Helligkeitszone, in Blendenstufen.",
    "Die Zone zählt ab Weiß nach der Belichtung: 0 EV sind die hellsten Lichter, −4 EV die Mitteltöne, −8 EV fast Schwarz. Nachbarzonen werden weich mitgenommen; benachbarte Regler daher ähnlich stellen, sonst kann der Tonverlauf kippen. Die Zonen kommen zum Ausgleich hinzu.",
    None,
);
static TONE_EQ_MASK: Card = card(
    "Stellt die Maske ein, die jede Bildstelle einer Zone zuordnet.",
    "• **Glättung** – wie groß die Flächen der Maske sind (Prozent der langen Bildkante). Mehr: ruhiger, Details behalten ihren Kontrast; weniger: wirkt eher wie eine Kurve.\n• **Kantenschärfe** – wie eng die Maske Kanten folgt. Höher: weniger Lichthöfe an Kanten, aber feine Strukturen trennen sich stärker.\n\nDie Maske wird je Einstellung einmal für das ganze Bild berechnet.",
    None,
);
static COLOR_CASTS: Card = card(
    "Nimmt Farbstiche aus ausgebrannten Lichtern.",
    "• **Lichter neutral** (nur Foveon) – nur in fast gesättigten hellen Stellen\n\nHelligkeit und kräftige Farben bleiben. Der Schalter im Kopf schaltet den Regler vorübergehend ab.",
    None,
);
static FILL_LIGHT: Card = card("", "Eine sehr weich gerechnete Helligkeitskarte zeigt, wo das Bild in dunkler Umgebung liegt; dort wird angehoben – für R, G und B gleich, die Farben bleiben. Ideal bei Gegenlicht und Innenräumen mit hellem Fenster; − drückt dunkle Bereiche weiter ab. Gerechnet wie das Fill Light der Kamera-Software (Retinex-Verfahren).", example_pair!("fill_light", "Vorher", "+1,0"));
static HIGHLIGHTS_NEUTRAL: Card = card("", "Beim Foveon-Sensor läuft die oberste Schicht zuerst voll. Nähert sie sich der Sättigung (ab etwa 60 % ihres Höchstwerts), stimmen die Farbverhältnisse nicht mehr, und helle Stellen bekommen einen Farbstich. Der Regler zieht diese Stellen gleitend zu Grau; dunklere Bildteile bleiben unberührt.", None);
static CLARITY: Card = card("", "Wirkt mit großem Radius (etwa 0,8 % der Bildbreite) nur auf die Helligkeit und schont Tiefen und Lichter. Gut für Holz, Rinde, Fell und Wolken; − macht weicher.", example_pair!("clarity", "Vorher", "+0,80"));
static VIBRANCE: Card = card("", "Bereits kräftige Farben werden kaum noch angehoben und laufen daher seltener aus als mit Sättigung. Negative Werte nehmen umgekehrt kräftige Farben stärker zurück.", example_pair!("vibrance", "Vorher", "+0,60"));
static SATURATION: Card = card("", "Wirkt auf alle Farben gleich stark; −1 ergibt Graustufen. Für Schwarzweiß besser „Schwarzweiß“ wählen – dort werden die Sensorschichten direkt gemischt.", example_pair!("saturation", "Vorher", "+0,40"));

// ── Curve, color mixer, black & white, color grading ──
static CURVE: Card = card("Punktkurve auf den fertigen Bildwerten nach der Tonkurve der Kamera.", "Eine sanfte S-Kurve gibt den Mitteltönen Kontrast; mit den Kanälen Rot, Grün und Blau lassen sich Farbstiche formen.\n\n### Tasten\nKlicken setzt einen Punkt · Ziehen verschiebt · Doppelklick entfernt", example_pair!("curve", "Vorher", "S-Kurve"));
static COLOR_MIXER: Card = card("Farbton, Sättigung und Helligkeit einzelner Farbbereiche.", "Jeder der acht Bereiche erfasst die Töne um seine Mitte und blendet weich in die Nachbarn über; Grautöne bleiben unberührt. Beispiel: Blau-Luminanz senkt den Himmel ab, Wolken und Laub bleiben.", example_pair!("color_mixer", "Vorher", "Blau-Luminanz −0,80"));
static BW_MIX: Card = card("Wie ein Farbfilter vor Schwarzweißfilm.", "Der Foveon misst an jedem Pixel drei Schichten: oben vor allem Blau, Mitte Grün, unten Rot. Für Schwarzweiß werden sie direkt gewichtet addiert – in voller Auflösung, zusammen immer 100 %.\n• **Viel „Unten“** – wie ein Rotfilter: blauer Himmel wird dunkel\n• **Dreieck** – Ecke = nur diese Schicht, Seitenmitte = zwei je zur Hälfte, Mitte = alle gleich\n\n### Tasten\n⌥ beim Ziehen: ohne Einrasten · Doppelklick: Mitte", example_pair!("bw_mix", "Summe", "Rotfilter"));
static LAYER_MIXER: Card = card("Verteilt die drei Foveon-Schichten frei auf Rot, Grün und Blau – statt der Farbmatrix der Kamera.", "Wie ein Kanalmixer, aber mit den Sensorschichten: Ausgabe Rot, Grün oder Blau wählen und festlegen, wie viel Prozent aus der unteren (≈ Rot/IR), mittleren (≈ Grün) und oberen Schicht (≈ Blau) kommen.\n• **Summe 100 %** – Grau bleibt neutral; mehr macht den Kanal heller\n• **Negative Anteile** – entmischen die überlappenden Schichten\n• **IR NRG** – Falschfarben-Infrarot (Vegetation rot)\n• **R↔B** – klassischer IR-Kanaltausch für blauen Himmel\n\n### Tipp\nFür Infrarot- und Vollspektrum-Umbauten; bei normalen Fotos auf „Sigma“ lassen.", None);
static GRADING: Card = card("Tönt Schatten, Mitteltöne und Lichter getrennt.", "Jedes Rad legt einen Farbton (Richtung) und dessen Stärke (Abstand zur Mitte) fest; die Helligkeit bleibt gleich. Klassisch: kühle Schatten, warme Lichter – oder Sepia und Selen für Schwarzweiß.\n\n### Bedienung\nRad ziehen: Farbton und Stärke · ⇧ beim Ziehen: nur die Stärke · Doppelklick: neutral", example_pair!("color_grading", "Vorher", "Split-Tone"));
static CALIBRATION: Card = card("Ändert die drei Grundfarben des Kameraprofils.", "• **Farbton** – dreht eine Grundfarbe zur nächsten (Rot → Gelb, Grün → Cyan, Blau → Violett)\n• **Sättigung** – macht sie bunter oder blasser\n• **Schatten-Tönung** – entfernt einen Grün- oder Magentastich in den Tiefen\n\nWirkt sanfter als der Farbmischer, wie ein anderes Profil. Werte aus Lightroom werden übernommen.\n\n### Tipp\nRot-Farbton etwas nach links für rosigere Haut, Blau-Sättigung höher für kräftigere Farben insgesamt.", None);
pub static RETOUCH: Card = card("Flecken entfernen und rote Augen korrigieren.", "• **Heilen** – Struktur von einer Quelle, an Helligkeit und Farbe der Umgebung angepasst (Sensorflecken, Pickel)\n• **Klonen** – kopiert die Quelle genau\n• **Rote Augen** – auf die Pupille klicken\n\nWerkzeug wählen, auf den Fleck klicken; die Quelle wählt graineo daneben, Ziehen beim Setzen legt sie selbst fest. Klick auf einen Kreis wählt den Punkt; Radius und Deckkraft wirken auf ihn.\n\n### Tasten\nH Punkte ein/aus · ⌫ gewählten löschen · Esc Werkzeug beenden", None);
static BALANCE: Card = card("", "Verschiebt die Grenze, ab der ein Ton als Schatten oder als Licht gilt. Wirkt nur, wenn mindestens ein Farbrad eine Stärke hat.", example_pair!("color_grading", "Vorher", "Split-Tone"));

// ── Effects ──
static VIGNETTE: Card = card("", "Bezieht sich auf den freigestellten Ausschnitt. Dezent eingesetzt lenkt sie den Blick zur Bildmitte.", example_pair!("vignette", "Vorher", "−0,60"));
static MIDPOINT: Card = card("", "Klein: Die Abdunklung beginnt nah an der Mitte und läuft breit aus. Groß: Nur die äußersten Ecken werden erfasst.", None);
static GRAIN: Card = card("", "Ein ortsfestes Muster, das in den Mitteltönen am stärksten ist und in Tiefschwarz und Weiß kaum. Es sitzt in Vorschau, 100 % und Export an derselben Stelle.", example_pair!("grain", "100 %", "Korn 0,50"));
static GRAIN_SIZE: Card = card("", "Die Größe bezieht sich auf die Bildbreite – das Korn wirkt bei jeder Ausgabegröße gleich grob.", None);
static ROUGHNESS: Card = card("", "Mischt eine zweite, halb so große Kornlage hinzu: Das Korn wirkt unregelmäßiger, eher wie Film als wie Rauschen.", None);
static MATTE: Card = card("", "Der dunkelste Ton wird zu einem Grau angehoben, alle anderen rücken anteilig nach. Wirkt nach Kurve und Color Grading – eine steile Kurve drückt ihn nicht wieder zu.", example_pair!("matte_shadows", "Vorher", "0,12"));

// ── Profile ──
static INTENSITY: Card = card(
    "",
    "0 % = ohne Film bzw. Look, 100 % = volle Wirkung, dazwischen stufenlos; auch das Korn wird schwächer.\n\nFarbmodi der Kamera, Neutral und Kamera-Looks sind Kameraprofile ohne Intensität – der Regler ist dann ausgegraut.",
    None,
);

// ── Lens corrections ──
static CA_PROFILE: Card = card("", "Verschiebt Rot- und Blaukanal mit Sigmas Messdaten zum Objektiv gegeneinander, damit farbige Ränder an Kanten zum Bildrand hin verschwinden. Gerechnet wird in der Foveon-Basis; nach dem Verstellen dauert es einige Sekunden.", None);
static CA_MANUAL: Card = card("", "Ohne Profil oder zum Nachstellen; gerechnet in der Foveon-Basis.\n\n### Tipp\nKanten nahe dem Bildrand bei 100 % ansehen und verstellen, bis rote/cyanfarbene bzw. blaue/gelbe Ränder verschwinden.", None);
static FRINGE_PURPLE: Card = card("", "Anders als die CA-Korrektur wird nichts verschoben: Entfärbt werden nur Pixel im gewählten Farbtonbereich an harten Hell-dunkel-Kanten; höhere Beträge wirken auch breiter. Bei 0 entfällt der Schritt ganz.", None);
static FRINGE_GREEN: Card = card("", "Wie bei den violetten Säumen nur an harten Kanten und nur im gewählten Farbtonbereich. Den Bereich schmal halten, wenn grüne Motive an Kanten mit entfärbt werden.", None);
static HUE: Card = card("", "Der Bereich ist auf der Spur hell markiert; Ziehen verschiebt den näheren Knopf. Enger erfasst nur genau diese Säume, weiter auch benachbarte Töne (mit weichem Übergang).", None);

// ── Crop & straighten ──
pub static CROP_ALIGN: Card = card(
    "Freistellen, Ausrichten und Perspektive.",
    "• **Freistellen** – Format wählen (Schloss hält das Seitenverhältnis), Rahmen ziehen; außerhalb ziehen dreht\n• **Ausrichten** – zwei Punkte am Horizont anklicken oder Auto, Waagerecht, Senkrecht\n• **Perspektive** – stürzende Linien entzerren\n\nAbbrechen stellt alles wie vor dem Öffnen her.\n\n### Tasten\n⇧ beim Ziehen einer Ecke: Seitenverhältnis halten · ⌘ Linie im Freistellen · A Ausrichten · Doppelklick in den Rahmen oder Enter: übernehmen · Esc Linie verwerfen",
    None,
);
static PERSPECTIVE_V: Card = card("", "Entzerrt um die Bildmitte, als wäre die Kamera nicht nach oben oder unten gekippt. Der Ausschnitt bleibt so groß wie möglich ohne leere Ecken.\n\n### Tipp\nGebäude von unten: nach links ziehen, bis die Kanten parallel stehen. „Senkrecht“ über den Reglern macht das automatisch.", None);
static PERSPECTIVE_H: Card = card("", "Wie senkrecht, nur für eine seitlich verdrehte Aufnahme: Waagerechte Kanten einer Fassade laufen dann parallel. Sparsam einsetzen – eine schräg gesehene Fassade wirkt sonst flach wie von vorn.", None);

// ── Detail ──
static SHARPNESS_SIGMA: Card = card("", "Schärfung auf einem Detailbild. Sie ändert nur die Helligkeit, nicht die Farbe, und hebt Foveon-Mikrodetails ohne Farbsäume hervor. Negativ nimmt die Grundschärfung zurück, etwa vor eigenem Schärfen.", example_pair!("sigma_sharpness", "200 %", "+2,0"));
static SHARPENING: Card = card("", "Unscharfe Maskierung auf der Helligkeit, nach allen Foveon-Schritten – etwa für den Druck oder starke Verkleinerung. Radius klein halten, sonst entstehen Lichthöfe.", example_pair!("sharpening", "200 %", "Betrag 1,0"));
static RADIUS: Card = card("", "Größere Werte erzeugen an kontrastreichen Kanten schneller helle oder dunkle Lichthöfe.", None);
static MASKING: Card = card("", "Bei 0 wird überall geschärft. Höhere Werte beschränken die Schärfung auf deutliche Helligkeitskanten – Rauschen in glatten Flächen bleibt unangetastet.", None);
static NR_LUMA: Card = card("Stärke der Helligkeits-Rauschunterdrückung, in fünf Stufen.", "• **Mitte** – Standard\n• **Weniger** – feinste Struktur mit etwas Korn\n• **Mehr** – glättet Flächen bei hoher ISO oder stark angehobenen Tiefen\n• **Ganz links** – kaum noch Glättung; die Grundschärfe bleibt und verstärkt das Korn\n\nJede Stufe rechnet die Foveon-Basis neu (einige Sekunden).", None);
static NR_COLOR: Card = card("Stärke der Farbrausch-Unterdrückung, in fünf Stufen.", "Farbrauschen zeigt sich beim Foveon als bunte Flecken in dunklen, gleichmäßigen Flächen. Mehr glättet sie, kann aber feine Farbdetails an Kanten verwischen.\n• **Mitte** – Standard\n• **Ganz links** – schwächste sinnvolle Stufe; ohne Farbglättung zeigen sich schon bei ISO 100 bunte Pünktchen\n\nJede Stufe rechnet die Basis neu.", None);

/// Card for a slider. `hint` distinguishes sliders with the same name (e.g. "Amount").
pub fn for_slider(label: &str, hint: &str) -> Option<&'static Card> {
    Some(match label {
        "Intensität" => &INTENSITY,
        "Druckauflösung" => &PRINT_RESOLUTION,
        "Ausdruck schärfen" => &PRINT_SHARPEN,
        "Farbraum der Seite" => &PRINT_COLOR,
        "Temperatur" if hint.contains("Kelvin") => &RAW_TEMPERATURE,
        "Tönung" if hint == "− grüner, + magenta" => &RAW_TINT,
        "Belichtung" if hint.contains("linear") => &RAW_EXPOSURE,
        "Kontrast" if hint.contains("Mitteltönen") => &RAW_CONTRAST,
        "Lichter" if hint.contains("örtlich") => &RAW_HIGHLIGHTS,
        "Tiefen" if hint.contains("örtlich") => &RAW_SHADOWS,
        "Weiß" => &RAW_WHITES,
        "Schwarz" => &RAW_BLACK,
        n if hint.contains("Foveon") && n == crate::detail_ui::fine_label() => &DETAIL_FINE,
        n if hint.contains("Foveon") && n == crate::detail_ui::structure_label() => &DETAIL_STRUCTURE,
        "Struktur" => &RAW_TEXTURE,
        "Luminanz" | "Luminanz-Detail" | "Farbe" | "Farb-Detail" if hint.to_lowercase().contains("rausch") || hint.contains("Höher") => &RAW_NR,
        "Verzeichnung" | "Vignettierung" => &RAW_LENS,
        "KI-Entrauschen" => &AI_DENOISE,
        "Stärke" if hint == "Anteil des KI-entrauschten Bildes" => &AI_DENOISE,
        "Temperatur" => &TEMPERATURE,
        "Tönung" => &TINT,
        "Belichtung" => &EXPOSURE,
        "Kontrast" => &CONTRAST,
        "Lichter" => &HIGHLIGHTS,
        "Tiefen" => &SHADOWS,
        "Fill Light (Retinex)" | "Fill Light" => &FILL_LIGHT,
        "Dunst entfernen" => &DEHAZE,
        _ if hint.starts_with(crate::tone_eq_ui::ZONE_HINT_START) => &TONE_EQ_ZONE,
        "Ausgleich" if hint == crate::tone_eq_ui::BALANCE_HINT => &TONE_EQ,
        "Glättung" | "Kantenschärfe" if hint.contains("Maske") => &TONE_EQ_MASK,
        "Lichter neutral" => &HIGHLIGHTS_NEUTRAL,
        "Binning" => &BINNING,
        "Rauschbremse" if !hint.is_empty() => &NOISE_BRAKE_RAW,
        "Rauschbremse" => &NOISE_BRAKE,
        "Klarheit" => &CLARITY,
        "Dynamik" => &VIBRANCE,
        "Sättigung" => &SATURATION,
        "Unten (rot)" | "Mitte (grün)" | "Oben (blau)" => &BW_MIX,
        "Balance" => &BALANCE,
        "aus Unten" | "aus Mitte" | "aus Oben" => &LAYER_MIXER,
        "Vignette" => &VIGNETTE,
        "Mittelpunkt" if hint.contains("Vignette") => &MIDPOINT,
        "Korn" if hint.contains("Filmkorn") => &FILM_GRAIN,
        "Korngröße" if hint.contains("Kornklumpen") => &FILM_GRAIN_SIZE,
        "Korngröße je Helligkeit" => &FILM_GRAIN_SECTION,
        "Korn" => &GRAIN,
        "Korngröße" => &GRAIN_SIZE,
        "Filmbelichtung" => &FILM_EXPOSURE,
        "Abzugshelligkeit" => &FILM_PRINT,
        "Abzug wärmer" => &FILM_WARMTH,
        "Abzug Tönung" => &FILM_TINT,
        "Filmkontrast" => &FILM_CONTRAST,
        "Papierkontrast" => &PAPER_CONTRAST,
        "Farbtrennung (DIR)" => &FILM_DIR,
        "Lichthof" => &FILM_HALATION,
        "Lichthof-Radius" => &FILM_HALATION_RADIUS,
        "Rauigkeit" => &ROUGHNESS,
        "Matte Tiefen" => &MATTE,
        "Stärke" if hint.contains("Objektivprofil") => &CA_PROFILE,
        "Rot" | "Blau" if hint.starts_with("Manuell: zieht") => &CA_MANUAL,
        "Rot" | "Orange" | "Gelb" | "Grün" | "Aquamarin" | "Blau" | "Lila" | "Magenta" if hint.contains("ereichs") => &COLOR_MIXER,
        "Betrag" if hint.contains("violette") => &FRINGE_PURPLE,
        "Betrag" if hint.contains("grüne") => &FRINGE_GREEN,
        "Betrag" if hint.starts_with("Zusätzlich") => &SHARPENING,
        "Farbton" if hint.contains("Saum") => &HUE,
        "Foveon-Schärfe" => &SHARPNESS_SIGMA,
        "Radius" if hint.contains("Kanten") => &RADIUS,
        "Maskieren" => &MASKING,
        "Senkrecht" if hint.starts_with("Perspektive") => &PERSPECTIVE_V,
        "Waagerecht" if hint.starts_with("Perspektive") => &PERSPECTIVE_H,
        "Luminanz" => &NR_LUMA,
        "Farbe" => &NR_COLOR,
        _ => return None,
    })
}

/// Card for a section (header).
pub fn for_section(title: &str) -> Option<&'static Card> {
    Some(match title {
        "Gradationskurve" => &CURVE,
        "Farbstiche" => &COLOR_CASTS,
        "Tonwert-Entzerrer" => &TONE_EQ,
        "Freistellen & Ausrichten" => &CROP_ALIGN,
        "Farbmischer" => &COLOR_MIXER,
        "Schichtmischer" => &LAYER_MIXER,
        "S/W-Mischung (Sensorschichten)" => &BW_MIX,
        "Color Grading" => &GRADING,
        "Kalibrierung" => &CALIBRATION,
        "Filmmaterial" => &FILM_MATERIAL,
        "Profil wählen" => &FILM_RENDERING,
        "Papier & Abzug" => &FILM_PAPER,
        "Belichtung & Dunkelkammer" => &FILM_DARKROOM,
        "Korn & Lichthof" => &FILM_GRAIN_SECTION,
        "Bibliothek" => &LIB_CATALOG,
        "Ordner" => &LIB_FOLDERS,
        "Sammlungen" => &LIB_COLLECTIONS,
        "Stichwörter" => &LIB_KEYWORDS,
        "Bewertung" => &LIB_RATING,
        "Metadaten" => &LIB_METADATA,
        "KI" => &BIB_AI,
        "Stichwörter bearbeiten" => &LIB_KEYWORDS,
        _ => return None,
    })
}

/// Card width: room for two thumbnails of 140 points each and about 55 characters of body text per line.
pub const WIDTH: f32 = 380.0;

/// Body text size (the interface's IBM Plex Mono) and line height of the cards.
const BODY: f32 = lr::UI_SIZE;
const LINE: f32 = 16.5;
/// Text of the set-off part at the end of a card (keys, tips).
const ASIDE: Color32 = Color32::from_gray(0x8a);

/// One line of body text as a layout job: `**…**` is set in the medium weight and the brighter text color.
fn body_job(text: &str, width: f32, color: Color32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width;
    let plain = egui::TextFormat { font_id: egui::FontId::proportional(BODY), color, line_height: Some(LINE), ..Default::default() };
    let strong = if color == ASIDE { Color32::from_gray(0xb0) } else { lr::TEXT() };
    let bold = egui::TextFormat { font_id: lr::semibold(BODY), color: strong, line_height: Some(LINE), ..Default::default() };
    for (n, part) in text.split("**").enumerate() {
        if !part.is_empty() {
            job.append(part, 0.0, if n % 2 == 1 { bold.clone() } else { plain.clone() });
        }
    }
    job
}

/// Body text of a card. Fixed scheme, all marks optional (plain text shows as one paragraph):
/// - a blank line starts a new paragraph,
/// - "• " or "- " at the start of a line is a list item with a hanging indent ("**Term** – explanation"),
/// - "## Heading" is a small heading in capitals,
/// - "### Heading" starts the set-off part at the end (keys, tips): a thin rule, the heading, dimmed text,
/// - `**…**` sets a term in the medium weight.
pub fn body(ui: &mut egui::Ui, text: &str, color: Color32) {
    let width = ui.available_width();
    const INDENT: f32 = 14.0;
    let mut color = color;
    let mut first = true;
    for paragraph in text.split("\n\n") {
        let paragraph = paragraph.trim_matches('\n');
        if paragraph.is_empty() {
            continue;
        }
        if !first {
            ui.add_space(7.0);
        }
        first = false;
        for line in paragraph.split('\n') {
            if let Some(heading) = line.strip_prefix("### ") {
                color = ASIDE;
                ui.add_space(2.0);
                let (r, _) = ui.allocate_exact_size(egui::vec2(width, 7.0), egui::Sense::hover());
                ui.painter().hline(r.x_range(), r.top(), egui::Stroke::new(1.0, Color32::from_gray(0x3a)));
                ui.label(egui::RichText::new(heading.to_uppercase()).font(lr::semibold(10.0)).color(ASIDE));
                continue;
            }
            if let Some(heading) = line.strip_prefix("## ") {
                ui.label(egui::RichText::new(heading.to_uppercase()).font(lr::semibold(10.0)).color(Color32::from_gray(0xb4)));
                continue;
            }
            let item = line.strip_prefix("• ").or_else(|| line.strip_prefix("- "));
            let indent = if item.is_some() { INDENT } else { 0.0 };
            let galley = ui.painter().layout_job(body_job(item.unwrap_or(line), width - indent, color));
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, galley.size().y), egui::Sense::hover());
            if item.is_some() {
                ui.painter().circle_filled(egui::pos2(rect.left() + 4.0, rect.top() + LINE * 0.5), 1.8, Color32::from_gray(0x8a));
            }
            ui.painter().galley(rect.min + egui::vec2(indent, 0.0), galley, color);
        }
    }
}

/// Thumbnail as texture – decoded once, then from the cache.
fn texture(ctx: &egui::Context, key: String, bytes: &[u8]) -> Option<egui::TextureHandle> {
    static CACHE: OnceLock<Mutex<HashMap<String, egui::TextureHandle>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    if let Some(t) = cache.get(&key) {
        return Some(t.clone());
    }
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).ok()?.to_rgba8();
    let color = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
    let tex = ctx.load_texture(crate::trf!("hilfe-{key}", key = key), color, egui::TextureOptions::LINEAR);
    cache.insert(key, tex.clone());
    Some(tex)
}

/// Card content: name, short hint + addition, example, usage (small).
pub fn show_card(ui: &mut egui::Ui, title: &str, hint: &str, card: &Card, controls: &str) {
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing.y = 3.0;
    ui.label(egui::RichText::new(crate::tr(title)).font(lr::semibold(13.0)).color(Color32::from_gray(0xf0)));
    ui.add_space(2.0);
    let short = if hint.is_empty() { card.short } else { hint };
    if !short.is_empty() {
        body(ui, crate::tr(short), lr::TEXT());
        ui.add_space(5.0);
    }
    body(ui, crate::tr(card.text), lr::TEXT_DIM());
    if let Some(extra) = card.extra {
        extra(ui);
    }
    if let Some(b) = &card.example {
        ui.add_space(6.0);
        let size = egui::vec2(140.0, 95.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            for (suffix, bytes, text) in [("before", b.before, b.before_label), ("after", b.after, b.after_label)] {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    match texture(ui.ctx(), format!("{}_{suffix}", b.name), bytes) {
                        Some(t) => {
                            ui.add(egui::Image::new(egui::load::SizedTexture::new(t.id(), size)).corner_radius(0.0));
                        }
                        None => {
                            ui.allocate_exact_size(size, egui::Sense::hover());
                        }
                    }
                    ui.label(egui::RichText::new(crate::tr(text)).small().color(lr::TEXT_DIM()));
                });
            }
        });
    }
    if !controls.is_empty() {
        ui.add_space(5.0);
        ui.label(egui::RichText::new(crate::tr(controls)).small().color(Color32::from_gray(0x9a)));
    }
}

/// Subtle "?" area (visible while `show` or the card is open). Only a click
/// opens the card; a second click, a click elsewhere or Esc closes it – merely
/// hovering does nothing. Only painted, takes no space in the layout (no jumps).
/// Is a help card open right now? (The HDR layer over the image would otherwise cover it.)
pub fn card_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<egui::Id>(egui::Id::unique("hilfe-offen"))).is_some()
}

pub fn question_mark(ui: &egui::Ui, center: egui::Pos2, id: egui::Id, show: bool, title: &str, hint: &str, card: &Card, controls: &str) {
    let open_id = egui::Id::unique("hilfe-offen");
    let mut open = ui.ctx().data(|d| d.get_temp::<egui::Id>(open_id)) == Some(id);
    if !open && crate::env::help_open_test(title, |w| crate::tr(w).to_string()) {
        ui.ctx().data_mut(|d| d.insert_temp(open_id, id));
        open = true;
    }
    if !show && !open {
        return;
    }
    let rect = egui::Rect::from_center_size(center, egui::vec2(14.0, 14.0));
    let r = ui.interact(rect, id, egui::Sense::click());
    let hovered = r.hovered() || open;
    let painter = ui.painter();
    painter.circle_stroke(center, 6.0, egui::Stroke::new(1.0, Color32::from_gray(if hovered { 0xd0 } else { 0x6a })));
    painter.text(center + egui::vec2(0.0, 0.5), egui::Align2::CENTER_CENTER, "?", egui::FontId::proportional(10.0), Color32::from_gray(if hovered { 0xf0 } else { 0x8c }));
    if r.clicked() {
        ui.ctx().data_mut(|d| {
            if open {
                d.remove::<egui::Id>(open_id);
            } else {
                d.insert_temp(open_id, id);
            }
        });
        return;
    }
    if !open {
        return;
    }
    // Card to the left of or below the icon, in the foreground
    let area = egui::Area::new(id.with("karte"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_TOP)
        .fixed_pos(rect.left_bottom() + egui::vec2(-4.0, 4.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| show_card(ui, title, hint, card, controls));
        });
    let outside = ui.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !rect.contains(p)));
    if outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.ctx().data_mut(|d| d.remove::<egui::Id>(open_id));
    }
}

/// Test hook for screenshots (`GRAINEO_HELP_CARD=<slider or section title>`): the card in the top left corner of
/// the window, also for controls that are not visible right now.
pub fn test_card(ctx: &egui::Context) {
    let Some(name) = crate::env::help_card_test() else { return };
    let Some(card) = for_slider(&name, "").or_else(|| for_section(&name)) else { return };
    egui::Area::new(egui::Id::new("hilfe-testkarte")).order(egui::Order::Foreground).fixed_pos(egui::pos2(330.0, 90.0)).show(ctx, |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| show_card(ui, &name, "", card, ""));
    });
}

/// Always visible "?" as a small element of its own in a row (e.g. after a
/// toggle); a click opens the card.
pub fn info(ui: &mut egui::Ui, salt: &str, title: &str, card: &Card) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
    let id = ui.scope_id().with(("hilfe-info", salt));
    question_mark(ui, rect.center(), id, true, crate::tr(title), "", card, "");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All embedded examples decode and are 280 × 190 pixels.
    #[test]
    fn examples_decode() {
        let all = [
            &TEMPERATURE, &TINT, &EXPOSURE, &CONTRAST, &HIGHLIGHTS, &SHADOWS, &FILL_LIGHT, &CLARITY, &VIBRANCE, &SATURATION, &CURVE, &COLOR_MIXER,
            &BW_MIX, &GRADING, &BALANCE, &VIGNETTE, &GRAIN, &MATTE, &SHARPNESS_SIGMA, &SHARPENING,
        ];
        for k in all {
            let b = k.example.as_ref().expect("example");
            for bytes in [b.before, b.after] {
                let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).expect(b.name);
                assert_eq!((img.width(), img.height()), (280, 190), "{}", b.name);
            }
        }
        // sliders with the same name are distinguished by the hint
        assert!(std::ptr::eq(for_slider("Betrag", "Entfärbt grüne Säume").unwrap(), &FRINGE_GREEN));
        assert!(std::ptr::eq(for_slider("Rot", "Manuell: zieht den Rotkanal").unwrap(), &CA_MANUAL));
        assert!(std::ptr::eq(for_slider("Rot", "Sättigung nur dieses Farbbereichs").unwrap(), &COLOR_MIXER));
        assert!(for_slider("Winkel", "Ausrichten").is_none());
    }
}

 pub static LOCAL_MASKS: Card=Card {short:"Masken",text:"Oben stehen alle Masken: das Auge schaltet eine aus, ⋯ benennt um, dupliziert, invertiert, verschiebt oder löscht. „+ Neue Maske ▾“ legt eine an: automatisch, als Form oder als Bereich.\n• **Teile** – eine Maske besteht aus Teilen: + hinzufügen, − abziehen, ∩ schneiden\n• **Werkzeug** – der gewählte Teil zeichnet im Bild: Pinsel malt, Verlauf und Ellipse an den Griffen ziehen, Farbbereich nimmt mit der Pipette Proben (⇧ dazu, ⌥ weg), Klick-Auswahl nimmt Klicks oder einen gezogenen Rahmen\n• **Kante** – Bitmap-Masken (Motiv, Personen, Klick-Auswahl) dehnen oder schrumpfen, ohne neu zu rechnen\n• **Tiefe** – wählt ganze Segmente nach ihrer Entfernung (Vorne, Mitte, Hinten); ein Klick ins Bild nimmt ein Segment dazu oder heraus\n• **Kante verfeinern** – gezeichnete Masken und Bereiche folgen in einem schmalen Band den Kanten im Bild\n• **Stärke** – Deckkraft der ganzen Maske\n• **Reihenfolge** – oben wird zuerst angewendet; Helligkeits- und Farbbereiche lesen den Stand davor\n\nDie Korrekturen folgen dem Bild auch nach Freistellen und Drehen.\n\n### Tasten\nH Konturen aus · ⇧O Überlagerung (Rot → Maske → Bild auf Grau → Abdeckfolie → aus) · Esc Werkzeug beenden · ⌫ Maske löschen · ⌘Z rückgängig",example:None,extra:None};

/// The split changes no stored edit or processing order.
pub static FILM_AND_LOOK: Card = card(
    "Film simuliert Filmmaterial, Entwicklung und Papier. Ein Look gestaltet Farbe und Tonwerte.",
    "• **Film** – simuliert Filmmaterial, Entwicklung, Papier, Korn und Lichthof\n• **Look** – gestaltet Farbe und Tonwerte; Looks aus Profilpaketen haben einen Intensitätsregler\n• **Wie Kamera** – der zum Bildstil der Aufnahme passende Look (z. B. Monochrom + Ye); wird nur auf Klick angewandt, unbearbeitete Fotos starten neutral\n\nFilm und Look schließen sich aus: Ein gewählter Look schaltet den Film aus, „Film ausschalten“ bringt den gespeicherten Look zurück.",
    None,
);

/// Photo comparison (photo_compare_ui.rs).
pub static COMPARE: Card = card(
    "Ausgewählte Fotos nebeneinander vergleichen und das beste wählen.",
    "Die Fotos kommen aus der Auswahl im Filmstreifen: ⌘-Klick nimmt eins hinzu oder heraus, Klick tauscht es gegen das fokussierte Feld.\n• **Vergleich** – bis zu 4 große Felder, mit der Anordnung „Alle“ bis zu 8 kleinere; Zoom und Verschieben wirken in allen Feldern gleich, ⇧-Ziehen verschiebt ein Feld einzeln; mehr Fotos blättern ‹ › oben rechts\n• **Turnier** – zwei Fotos je Duell bis Platz 1–3; danach steht die Rangliste in der Anordnung „Alle“ (Platz-Marken 1., 2., 3.), gesetzt wird erst mit „Übernehmen“: Platz 1 wird Auswahl und Titelfoto, alle anderen werden abgelehnt (ein ⌘Z)\n\nDie Hinweise sind relativ zur Auswahl: Schärfe als Anteil des schärfsten Fotos, Augen und Gesichter (Apple Vision, nur macOS), abgeschnittene Lichter, Bewegungsunschärfe. Der Vorschlag nennt seinen Grund und setzt nichts.\n\n### Tasten\n↩ bestes wählen · ⇧X Rest ablehnen · P/X/U/0–5 wirken auf das fokussierte Feld · Z Zoom · Leertaste halten 100 % an der Maus · L gekoppelt · H Gesichter · ⌘↩ gesichtet, weiter · Esc zurück",
    None,
);

/// Look comparison in "Film & Look" (compare_view.rs).
pub static LOOK_COMPARE: Card = card(
    "Looks nebeneinander vergleichen.",
    "Vergleichsknopf (zwei Felder, oben rechts) einschalten und bis zu drei Look-Kacheln anklicken; ein zweiter Klick nimmt einen wieder heraus.\n• **Erstes Feld** – der aktuelle Stand\n• **Weitere Felder** – derselbe Stand mit dem gewählten Look\n\nZoomen und Verschieben wirken in allen Feldern gleich. Ein Klick auf ein Feld übernimmt dessen Look.\n\n### Tasten\n⌘-Klick vergleicht ohne Schalter · ⌘Z rückgängig · Esc beendet",
    None,
);

/// Choice of the search model (Settings → Search, prefs_search_model.rs). Numbers: own measurement
/// (XTD10, Rust implementation); placeholders until measured.
pub static SEARCH_MODEL: Card = card(
    "Das Suchmodell vergleicht Suchwörter mit dem Inhalt der Fotos.",
    "• **Standard** – wenig Speicher, liest neue Fotos schnell ein, versteht Deutsch schwächer\n• **Mehrsprachig** – größeres Text-Modell: versteht Deutsch deutlich besser, die Fotos bleiben eingelesen\n• **Beste Treffer** – neues Bild- und Text-Modell: am genauesten, alle Fotos werden einmal neu eingelesen\n\nEin neues Bildmodell liest alle Fotos einmal im Hintergrund ein; die Suche nutzt bis dahin das bisherige. Zurückwechseln geht sofort, eingelesene Fotos bleiben erhalten.\n\n### Qualität\nDie Skala folgt einer eigenen Messung: 1000 Fotos mit je einer kurzen Beschreibung (Testsatz XTD10); gezählt wird, wie oft das gesuchte Foto auf Platz 1 steht (Werte in der Karte jedes Modells). Die deutschen Beschreibungen sind teils maschinell übersetzt; bei eigenen Fotos liegen die Werte anders, die Reihenfolge bleibt.\n\n### Tempo\nGemessen auf einem Mac mit M1 Pro. Das Einlesen läuft gedrosselt mit halber Kraft, damit der Mac leise bleibt.",
    None,
);

/// Mark for the conditions of the shown photo (see `photo_state`) in a row: a muted yellow "!" in a circle, only
/// while `items` (title, explanation) are present; the space is always taken, so nothing moves. A click opens a
/// card above it with `heading` and the explanations, closed like the "?" cards (click again, elsewhere, Esc).
/// One condition in the card of [`notice_mark`].
pub struct NoticeItem {
    pub title: String,
    pub text: String,
    /// share of the rows really read per sensor layer (bottom, middle, top): one small bar per layer
    pub layers: Option<[f64; 3]>,
}

/// Bars "share read" per sensor layer, top layer first.
fn layer_bars(ui: &mut egui::Ui, read: [f64; 3], color: Color32) {
    for (p, name) in [(2, "Obere Schicht"), (1, "Mittlere Schicht"), (0, "Untere Schicht")] {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), LINE), egui::Sense::hover());
        let painter = ui.painter();
        let font = egui::FontId::proportional(BODY - 0.5);
        painter.text(rect.left_center(), egui::Align2::LEFT_CENTER, crate::tr(name), font.clone(), lr::TEXT_DIM());
        let pct = (read[p] * 100.0).round() as i64;
        painter.text(rect.right_center(), egui::Align2::RIGHT_CENTER, crate::trf!("{a} % gelesen", a = pct), font, lr::TEXT_DIM());
        let bar = egui::Rect::from_min_size(egui::pos2(rect.left() + 118.0, rect.center().y - 2.5), egui::vec2(rect.width() - 118.0 - 92.0, 5.0));
        painter.rect_filled(bar, 0.0, Color32::from_gray(0x3a));
        let mut done = bar;
        done.set_width(bar.width() * read[p].clamp(0.0, 1.0) as f32);
        painter.rect_filled(done, 0.0, color);
    }
}

pub fn notice_mark(ui: &mut egui::Ui, salt: &str, heading: &str, items: &[NoticeItem], color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::hover());
    let open_id = egui::Id::unique("hilfe-offen");
    let id = egui::Id::unique(("zustand-marke", salt));
    let mut open = ui.ctx().data(|d| d.get_temp::<egui::Id>(open_id)) == Some(id);
    if !open && !items.is_empty() && crate::env::help_open_test("!", |w| crate::tr(w).to_string()) {
        ui.ctx().data_mut(|d| d.insert_temp(open_id, id));
        open = true;
    }
    if items.is_empty() {
        if open {
            ui.ctx().data_mut(|d| d.remove::<egui::Id>(open_id));
        }
        return;
    }
    let center = rect.center();
    let r = ui.interact(rect, id, egui::Sense::click());
    let strong = r.hovered() || open;
    let c = if strong { color } else { color.gamma_multiply(0.85) };
    let painter = ui.painter();
    painter.circle_stroke(center, 7.0, egui::Stroke::new(1.2, c));
    painter.text(center + egui::vec2(0.0, 0.5), egui::Align2::CENTER_CENTER, "!", egui::FontId::proportional(11.0), c);
    if r.clicked() {
        ui.ctx().data_mut(|d| {
            if open {
                d.remove::<egui::Id>(open_id);
            } else {
                d.insert_temp(open_id, id);
            }
        });
        return;
    }
    if !open {
        return;
    }
    let area = egui::Area::new(id.with("karte"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(rect.right_top() + egui::vec2(8.0, -6.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(WIDTH);
                ui.spacing_mut().item_spacing.y = 3.0;
                ui.label(egui::RichText::new(heading).font(lr::semibold(13.0)).color(Color32::from_gray(0xf0)));
                for item in items {
                    ui.add_space(5.0);
                    ui.label(egui::RichText::new(&item.title).font(lr::semibold(BODY)).color(color));
                    body(ui, &item.text, lr::TEXT_DIM());
                    if let Some(read) = item.layers {
                        ui.add_space(4.0);
                        layer_bars(ui, read, color);
                    }
                }
            });
        });
    let outside = ui.input(|i| i.pointer.any_click() && i.pointer.interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !rect.contains(p)));
    if outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.ctx().data_mut(|d| d.remove::<egui::Id>(open_id));
    }
}

/// Subject and people masks (bitmap masks from the system's image analysis).
pub static SUBJECT_MASKS: Card = card(
    "„Motiv“ und „Personen“ wählen mit der Bildanalyse des Systems aus, was im Foto vorne steht. Die Maske ist nur eine Auswahl: Pixel ändern erst die Regler darunter.",
    "• **Motiv 1, 2 …** – mehrere erkannte Motive einzeln an- und abwählen\n• **Weiche Kante** – weicher Übergang am Rand der Auswahl\n• **+ Hinzufügen / − Abziehen → Pinsel** – vergessene Teile ergänzen, zu viel Ausgewähltes abziehen\n• **Überlagerung** – zeigt die Wirkung der Maske rot im Bild\n\nGrenzen: Haare, Fell, Glas, feine Zweige und Bilder ohne klares Hauptmotiv werden oft ungenau. Die Berechnung läuft auf dem Rechner, nichts verlässt ihn. Die Auswahl wird in der Bibliothek gespeichert; beim Einfügen auf andere Fotos wird sie dort neu berechnet.\n\nNur unter macOS (Motiv: ab macOS 14).",
    None,
);

/// Removing objects (inpainting model LaMa).
pub static REMOVE_OBJECTS: Card = card(
    "„Stelle entfernen“ füllt eine angeklickte Stelle mit Inhalt, den ein Modell aus der Umgebung erfindet. Das ist keine Fotografie mehr, sondern eine Vermutung.",
    "• **Gut** – Himmel, Wasser, Wiese, Wand, Muster\n• **Schlecht** – Gesichter, Schrift, Kanten von Gegenständen, große Flächen\n• **Größe** – Radius des nächsten Kreises\n\nDie Füllung wird einmal berechnet und in der Bibliothek gespeichert; Helligkeit und Farbe passen sich beim Entwickeln der Umgebung an. Für kleine Flecken auf echtem Bildinhalt ist „Heilen“ meist besser. Das Modell („Objekte entfernen“, Einstellungen → Module) rechnet auf dem Rechner, nichts verlässt ihn.",
    None,
);
