//! Generic entity cleanup helper used by every sketch's `OnExit` handler.

use bevy::prelude::*;

/// Despawn every entity tagged with marker `M`.
///
/// Wire this into a sketch's `OnExit(AppState::X)` schedule:
///
/// ```ignore
/// app.add_systems(OnExit(AppState::Line), despawn_with::<LineRoot>);
/// ```
///
/// Cascading: entity children are despawned alongside their parents
/// (Bevy 0.14+ makes `despawn` cascade by default — children of any
/// despawned entity are also freed).
pub fn despawn_with<M: Component>(
    mut commands: Commands<'_, '_>,
    query: Query<'_, '_, Entity, With<M>>,
) {
    for entity in &query {
        commands.entity(entity).despawn();
    }
}
