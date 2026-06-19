//! Prune per-image adjustments for templates that no longer exist.
//!
//! When an image is deleted from the template cache (or removed out-of-band),
//! its [`LineTemplateAdjustments`] entry is dead weight. This drops map entries
//! whose hash is no longer in the [`TemplateLibrary`], keeping the persisted map
//! in sync with the store. It runs when the library changes (which includes the
//! startup insert + reconcile, healing out-of-band deletes), and only takes the
//! map mutably — arming autosave — when a removal is actually needed.

#![cfg(feature = "templates")]

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use wc_core::templates::resource::TemplateLibrary;

use crate::line::template_adjustments::TemplateAdjustments;
use crate::line::template_adjustments_store::LineTemplateAdjustments;

/// Remove entries whose hash is not in `live_hashes`. Returns whether anything
/// was removed.
#[allow(
    clippy::implicit_hasher,
    reason = "internal helper; only called with the std-hasher map from LineTemplateAdjustments"
)]
pub fn prune(map: &mut HashMap<String, TemplateAdjustments>, live_hashes: &HashSet<&str>) -> bool {
    let before = map.len();
    map.retain(|hash, _| live_hashes.contains(hash.as_str()));
    map.len() != before
}

/// Drop adjustments for images no longer in the template library. Reads the map
/// immutably to decide whether a removal is needed, taking `&mut` (which arms
/// autosave) only when there is actually an orphan to prune.
pub fn prune_orphan_adjustments(
    library: Res<'_, TemplateLibrary>,
    mut adjustments: ResMut<'_, LineTemplateAdjustments>,
) {
    let live: HashSet<&str> = library.entries.iter().map(|e| e.hash.as_str()).collect();
    // Immutable peek (Res/ResMut Deref does not mark changed); only DerefMut in
    // `prune` arms autosave, and only when an orphan exists.
    let has_orphan = adjustments.map.keys().any(|h| !live.contains(h.as_str()));
    if has_orphan {
        prune(&mut adjustments.map, &live);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_drops_orphans_only() {
        let mut map = HashMap::new();
        map.insert("keep".to_string(), TemplateAdjustments::default());
        map.insert("gone".to_string(), TemplateAdjustments::default());
        let live: HashSet<&str> = ["keep"].into_iter().collect();
        assert!(prune(&mut map, &live));
        assert!(map.contains_key("keep") && !map.contains_key("gone"));
    }

    #[test]
    fn prune_noop_when_all_live() {
        let mut map = HashMap::new();
        map.insert("a".to_string(), TemplateAdjustments::default());
        let live: HashSet<&str> = ["a", "b"].into_iter().collect();
        assert!(!prune(&mut map, &live));
        assert_eq!(map.len(), 1);
    }
}
