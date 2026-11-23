//! Which Sigma base may be shown for the settings now asked for.
//!
//! The exact Sigma base takes seconds. If the settings change while it is computed, the result that arrives
//! belongs to the old settings: it is never shown (otherwise the picture jumps back to the old state before
//! the base for the new settings follows). A base already shown stays in use for small changes – colour mode,
//! noise and CA sliders are converted from it – but never across the switch between colour and the grey base of
//! black & white: a grey base renders grey whatever the settings say, a colour base cannot give the grey of the
//! Sigma way. Until the fitting base is there, the fast preview with the new settings is shown (where that preview is
//! too rough to be shown, the picture on screen stays instead, see `preview_hold.rs`). A base still being
//! computed for settings changed meanwhile is cancelled (finished anyway, it stays in the disk cache under its key,
//! by the cache's rules); the one for the new settings starts once they have rested for [`SETTLE_SECS`].

use develop::SppKey;

/// Rest after a change that needs another base before that base is started (seconds): a quick back and forth
/// (colour mode, treatment) starts no base; the fast preview shows each step at once.
pub(crate) const SETTLE_SECS: f64 = 0.25;

/// What to do with a finished base for the photo on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Arrival {
    /// Made for the settings asked for now: show it
    Show,
    /// Made for settings changed meanwhile: never shown (the base for the settings now is asked for instead)
    Outdated,
}

/// Decide about a base `arrived` for the settings whose key is `wanted` now.
pub(crate) fn arrival(arrived: &SppKey, wanted: &SppKey) -> Arrival {
    if arrived == wanted { Arrival::Show } else { Arrival::Outdated }
}

/// May a base made for `base` stand in for the settings with key `wanted` until their own base is there?
/// Yes for the same kind of base (colour mode, noise and CA differ: converted linearly as before), no across
/// colour ↔ black & white and for another layer mix of the grey base (the mix is part of the grey values).
pub(crate) fn stands_in(base: &SppKey, wanted: &SppKey) -> bool {
    base.mono == wanted.mono && (!base.mono || base.mono_mix == wanted.mono_mix) && base.prepared == wanted.prepared && base.binning == wanted.binning
}

/// Next step for the base of the photo on screen, decided each frame from the base on its way (`pending`, already
/// for the settings now – one for older settings is cancelled before), the base shown, the settings waiting to rest
/// (`settling`) and the key of the settings now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Nothing to do: the base is on its way, or the one shown fits
    Wait,
    /// Back to the base on screen before the settings rested: the waiting start is dropped
    BackToShown,
    /// Another base needed: start it once the settings have rested ([`SETTLE_SECS`] after the last change)
    Settle,
    /// No base and no change (opened without one, noise slider back): ask for it as before
    Plain,
}

pub(crate) fn step(pending: bool, shown: Option<&SppKey>, settling: Option<&SppKey>, wanted: &SppKey) -> Step {
    if pending {
        Step::Wait
    } else if shown == Some(wanted) {
        if settling.is_some() { Step::BackToShown } else { Step::Wait }
    } else if shown.is_some() || settling.is_some() {
        Step::Settle
    } else {
        Step::Plain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(mono: bool, mode: &str) -> SppKey {
        let s = develop::Settings { monochrome: mono, color_mode: Some(mode.into()), ..Default::default() };
        s.spp_key_class(false)
    }

    /// The case reported: photo shot in black & white, treatment switched to colour while the grey base is
    /// computed – the grey base arriving afterwards must not be shown.
    #[test]
    fn outdated_base_is_not_shown() {
        let grey = key(true, "Standard");
        let colour = key(false, "Standard");
        assert_eq!(arrival(&grey, &colour), Arrival::Outdated);
        assert_eq!(arrival(&colour, &grey), Arrival::Outdated);
        assert_eq!(arrival(&colour, &colour), Arrival::Show);
        assert_eq!(arrival(&grey, &grey), Arrival::Show);
        // another colour mode or noise slider: outdated as well
        assert_eq!(arrival(&key(false, "Vivid"), &colour), Arrival::Outdated);
        let mut nr = colour.clone();
        nr.nr.0 += 1;
        assert_eq!(arrival(&nr, &colour), Arrival::Outdated);
    }

    /// A base on screen stands in only for the same kind of base.
    #[test]
    fn stand_in_only_same_kind() {
        let grey = key(true, "Standard");
        let colour = key(false, "Standard");
        assert!(!stands_in(&grey, &colour), "grey base shown for colour settings");
        assert!(!stands_in(&colour, &grey), "colour base shown for the grey base");
        assert!(stands_in(&key(false, "Vivid"), &colour), "colour mode is converted from the base");
        let mut nr = colour.clone();
        nr.nr = (0, 0);
        assert!(stands_in(&nr, &colour), "noise sliders: the base stays until the new one is there");
        let mut mix = grey.clone();
        mix.mono_mix = [500, 250, 250];
        assert!(!stands_in(&mix, &grey), "other layer mix of the grey base");
        assert!(stands_in(&grey, &grey));
    }

    /// Quick back and forth: while the settings change, no base starts; back to the base shown, nothing starts.
    #[test]
    fn back_and_forth_starts_nothing() {
        let grey = key(true, "Standard");
        let colour = key(false, "Standard");
        let vivid = key(false, "Vivid");
        // colour base shown, colour mode changed: wait for rest
        assert_eq!(step(false, Some(&colour), None, &vivid), Step::Settle);
        // and back before the rest is over: nothing started
        assert_eq!(step(false, Some(&colour), Some(&vivid), &colour), Step::BackToShown);
        assert_eq!(step(false, Some(&colour), None, &colour), Step::Wait);
        // grey base dropped for colour settings (nothing shown, settling): still waiting for rest, not asked at once
        assert_eq!(step(false, None, Some(&colour), &colour), Step::Settle);
        assert_eq!(step(false, None, Some(&colour), &grey), Step::Settle);
        // base for the settings now on its way
        assert_eq!(step(true, None, None, &colour), Step::Wait);
        // opened without a base, nothing changed
        assert_eq!(step(false, None, None, &colour), Step::Plain);
    }
}
