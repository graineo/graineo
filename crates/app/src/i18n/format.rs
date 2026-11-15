//! Display-only formats. EXIF, JSON, XMP and photo settings never call these helpers.
use super::Language;

pub fn decimal(language: Language, value: f64, places: usize) -> String {
    let text = format!("{:.*}", places.min(12), value);
    if language.decimal_comma() {
        text.replace('.', ",")
    } else {
        text
    }
}
pub fn integer(language: Language, value: usize) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i > 0 && (text.len() - i) % 3 == 0 {
            out.push(match language {
                Language::De | Language::Fr => '\u{202f}',
                Language::Es => '\u{a0}',
                Language::En | Language::Ja => ',',
            });
        }
        out.push(ch);
    }
    out
}
/// Preserve the existing dialog::groesse German output, including its 1 MB minimum.
pub fn size_decimal(language: Language, bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{} GB", decimal(language, bytes as f64 / 1e9, 1))
    } else {
        format!("{} MB", decimal(language, (bytes as f64 / 1e6).max(1.0), 0))
    }
}
pub fn size_binary(language: Language, bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!(
            "{} GiB",
            decimal(language, bytes as f64 / (1u64 << 30) as f64, 1)
        )
    } else {
        format!(
            "{} MiB",
            decimal(language, bytes as f64 / (1u64 << 20) as f64, 1)
        )
    }
}
#[derive(Clone, Copy)]
pub enum Unit {
    Ev,
    Millimetres,
    Seconds,
    Degrees,
    Percent,
}
pub fn unit(language: Language, value: f64, places: usize, unit: Unit) -> String {
    let suffix = match unit {
        Unit::Ev => " EV",
        Unit::Millimetres => " mm",
        Unit::Seconds => " s",
        Unit::Degrees => "°",
        Unit::Percent => " %",
    };
    format!("{}{suffix}", decimal(language, value, places))
}
/// Camera date parts, without inventing a time zone or converting the stored EXIF value.
pub fn date(language: Language, year: i32, month: u8, day: u8) -> Option<String> {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if !(1..=9999).contains(&year) || day == 0 || day > days {
        return None;
    }
    Some(match language {
        Language::De => format!("{day:02}.{month:02}.{year:04}"),
        Language::Fr | Language::Es => format!("{day:02}/{month:02}/{year:04}"),
        Language::Ja => format!("{year:04}/{month:02}/{day:02}"),
        Language::En => format!("{year:04}-{month:02}-{day:02}"),
    })
}
pub fn time(hour: u8, minute: u8, second: Option<u8>) -> Option<String> {
    if hour > 23 || minute > 59 || second.is_some_and(|s| s > 59) {
        return None;
    }
    Some(match second {
        Some(s) => format!("{hour:02}:{minute:02}:{s:02}"),
        None => format!("{hour:02}:{minute:02}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn german_legacy_numbers_remain_exact() {
        for n in [0usize, 1, 2, 999, 1000, 45337, 1_000_000] {
            let s = n.to_string();
            let mut old = String::new();
            for (i, c) in s.chars().enumerate() {
                if i > 0 && (s.len() - i) % 3 == 0 {
                    old.push('\u{202f}');
                }
                old.push(c);
            }
            assert_eq!(integer(Language::De, n).as_bytes(), old.as_bytes());
        }
        for bytes in [
            0,
            1,
            900_000,
            1_000_000,
            45_337_000,
            999_999_999,
            1_000_000_000,
            u64::MAX,
        ] {
            let old = if bytes >= 1_000_000_000 {
                format!("{:.1} GB", bytes as f64 / 1e9)
            } else {
                format!("{:.0} MB", (bytes as f64 / 1e6).max(1.0))
            }
            .replace('.', ",");
            assert_eq!(size_decimal(Language::De, bytes).as_bytes(), old.as_bytes());
        }
        assert_eq!(decimal(Language::De, -1.25, 2), "-1,25");
        assert_eq!(decimal(Language::En, -1.25, 2), "-1.25");
        assert_eq!(size_binary(Language::En, 1 << 30), "1.0 GiB");
        assert_eq!(unit(Language::De, 12.5, 1, Unit::Percent), "12,5 %");
        assert_eq!(unit(Language::En, 35.0, 0, Unit::Millimetres), "35 mm");
        assert_eq!(unit(Language::De, 90.0, 0, Unit::Degrees), "90°");
        assert_eq!(unit(Language::En, 0.5, 1, Unit::Ev), "0.5 EV");
        assert_eq!(unit(Language::De, 1.25, 2, Unit::Seconds), "1,25 s");
    }
    #[test]
    fn dates_and_times_are_display_values_only() {
        assert_eq!(
            date(Language::De, 2024, 2, 29).as_deref(),
            Some("29.02.2024")
        );
        assert_eq!(
            date(Language::En, 2024, 2, 29).as_deref(),
            Some("2024-02-29")
        );
        assert_eq!(date(Language::Fr, 2024, 2, 29).as_deref(), Some("29/02/2024"));
        assert_eq!(date(Language::Ja, 2024, 2, 29).as_deref(), Some("2024/02/29"));
        assert_eq!(decimal(Language::Es, 1.5, 1), "1,5");
        assert_eq!(decimal(Language::Ja, 1.5, 1), "1.5");
        assert_eq!(integer(Language::Fr, 45337), "45\u{202f}337");
        assert!(date(Language::De, 2025, 2, 29).is_none());
        assert!(date(Language::De, 2025, 0, 1).is_none());
        assert_eq!(time(14, 3, None).as_deref(), Some("14:03"));
        assert_eq!(time(14, 3, Some(22)).as_deref(), Some("14:03:22"));
        assert!(time(24, 0, None).is_none());
    }
}
