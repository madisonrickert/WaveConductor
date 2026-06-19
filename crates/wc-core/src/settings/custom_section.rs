//! A render-only escape hatch for sketch-contributed settings UI.
//!
//! The reflection-driven panel ([`super::panel_user`]) renders every flat
//! [`super::trait_def::SketchSettings`] field automatically. Some settings,
//! though, cannot be expressed as a fixed table of named fields — e.g. a
//! variable-cardinality, hash-keyed map of sub-records (the Line per-image
//! template adjustments). For exactly those, a sketch can register a render
//! callback here that draws straight into the settings dock.
//!
//! ## This hook renders; it does NOT persist
//!
//! Standard flat settings MUST use `#[derive(SketchSettings)]` — that is what
//! buys persistence, autosave, `requires_restart` diffing, reset-to-default,
//! modified-highlighting, and dev-console addressing. A `DockSectionFn` provides
//! none of those. The state a custom section edits MUST live in a **registered**
//! [`super::trait_def::SketchSettings`] resource (an empty `settings_def` is
//! fine) so persistence/autosave/change-detection stay centralized. Mutate that
//! resource through `world.get_resource_mut::<…>()` from the callback; that arms
//! the existing debounce. Do not invent a parallel persistence path here.

use bevy::prelude::*;
use bevy_egui::egui;

use crate::ui::OverlayStyle;

/// A sketch-contributed dock-section renderer. It re-enters the `World` (the
/// dock runs as an exclusive system) to read/write its registered resource and
/// draws into `ui`. Function pointers (not boxed closures) keep registration
/// allocation-free and `Copy` so the dock can snapshot them without holding the
/// [`CustomDockSections`] borrow across the `World` re-entry.
pub type DockSectionFn = fn(&mut World, &mut egui::Ui, &OverlayStyle);

/// Registry of custom dock sections, each keyed by the `STORAGE_KEY` of the
/// settings section it renders **after**. A section therefore appears in the
/// same tab as that key's reflected section, immediately below it.
#[derive(Resource, Default)]
pub struct CustomDockSections {
    entries: Vec<(&'static str, DockSectionFn)>,
}

impl CustomDockSections {
    /// Register `render` to draw immediately after the reflected section for
    /// `after_key` (a registered settings `STORAGE_KEY`).
    pub fn register(&mut self, after_key: &'static str, render: DockSectionFn) {
        self.entries.push((after_key, render));
    }

    /// The renderers registered after `key`, in registration order.
    pub fn for_key<'a>(&'a self, key: &'a str) -> impl Iterator<Item = DockSectionFn> + 'a {
        self.entries
            .iter()
            .filter(move |(k, _)| *k == key)
            .map(|(_, f)| *f)
    }
}

/// Extension trait adding [`App::register_dock_section`] for sketches to
/// contribute a custom dock section.
pub trait RegisterDockSectionExt {
    /// Register `render` to draw after the dock section for `after_key`. See the
    /// module docs: the hook renders only; back its state with a registered
    /// [`super::trait_def::SketchSettings`] resource.
    fn register_dock_section(
        &mut self,
        after_key: &'static str,
        render: DockSectionFn,
    ) -> &mut Self;
}

impl RegisterDockSectionExt for App {
    fn register_dock_section(
        &mut self,
        after_key: &'static str,
        render: DockSectionFn,
    ) -> &mut Self {
        self.world_mut()
            .get_resource_or_insert_with(CustomDockSections::default)
            .register(after_key, render);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop(_: &mut World, _: &mut egui::Ui, _: &OverlayStyle) {}

    #[test]
    fn sections_are_keyed_and_ordered() {
        let mut s = CustomDockSections::default();
        s.register("line", noop);
        s.register("line", noop);
        s.register("display", noop);
        assert_eq!(s.for_key("line").count(), 2);
        assert_eq!(s.for_key("display").count(), 1);
        assert_eq!(s.for_key("hand_tracking").count(), 0);
    }
}
