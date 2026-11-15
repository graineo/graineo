use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("E/A-Fehler: {0}")]
    Io(#[from] std::io::Error),
    #[error("keine X3F-Datei (Kennung {0:?})")]
    NotX3f([u8; 4]),
    #[error("Datei beschädigt: {0}")]
    Corrupt(String),
    /// The compressed RAW data contradicts itself (overwritten or missing parts of the file,
    /// e.g. from a damaged memory card). Header, previews and metadata may still be intact.
    /// `plane`: 0 … 2 (top to bottom layer); `first_row`: first unreadable row (`None`: the plane
    /// needs more data than the file holds); `rows`: rows of the plane.
    #[error("Rohdaten beschädigt (Ebene {}, {})", plane + 1, first_row.map_or("Ende fehlt".to_string(), |r| format!("ab Zeile {r} von {rows}")))]
    Damaged { plane: usize, first_row: Option<usize>, rows: usize },
    #[error("nicht unterstützt: {0}")]
    Unsupported(String),
    #[error("CAMF-Eintrag fehlt: {0}")]
    MissingCamf(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn corrupt<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Corrupt(msg.into()))
}
