//! Keyed children: keeps the entities under some parents in step with a list of keyed view models, so a redraw
//! changes only what changed instead of despawning and respawning everything (which flashes, and drops scroll
//! positions with the nodes that held them).
//!
//! [`Keyed::sync`] is handed, per parent, the models that parent should hold in order. An entity is spawned for a
//! key it has not seen, updated when its model differs from the last one, moved (with the parent's children
//! reordered, never respawned) when its parent or place changed, and despawned when its key is gone. A key that
//! moves from one parent to another keeps its entity. The parents must hold nothing but these entities.
//!
//! The helper only issues commands. It knows nothing of how an entity is drawn: `spawn` and `update` put the model on
//! the entity, and a system watching for it draws.

use std::collections::HashMap;

use bevy::prelude::*;

/// The entities of one family of keyed children (all the cards of a board, say) and the models they were last
/// given.
pub struct Keyed<M> {
    entries: HashMap<String, (Entity, M)>,
    orders: HashMap<Entity, Vec<Entity>>,
}

impl<M> Default for Keyed<M> {
    fn default() -> Self {
        Keyed { entries: HashMap::new(), orders: HashMap::new() }
    }
}

impl<M: Clone + PartialEq> Keyed<M> {
    /// The entity for `key`, if there is one.
    pub fn entity(&self, key: &str) -> Option<Entity> {
        self.entries.get(key).map(|(e, _)| *e)
    }

    /// How many keys have an entity.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forgets everything without despawning, for when the parents are gone and took their children with them.
    pub fn forget(&mut self) {
        self.entries.clear();
        self.orders.clear();
    }

    /// Brings the children of each parent to `wanted`: `(parent, [(key, model)])`.
    ///
    /// `spawn` makes the entity for a new key (it should not parent it); `update` runs when a known key's model
    /// changed.
    pub fn sync(
        &mut self,
        commands: &mut Commands,
        wanted: &[(Entity, Vec<(String, M)>)],
        mut spawn: impl FnMut(&mut Commands, &str, &M) -> Entity,
        mut update: impl FnMut(&mut Commands, Entity, &M),
    ) {
        let keep: std::collections::HashSet<&str> =
            wanted.iter().flat_map(|(_, items)| items.iter().map(|(k, _)| k.as_str())).collect();
        self.entries.retain(|key, (entity, _)| {
            let kept = keep.contains(key.as_str());
            if !kept {
                commands.entity(*entity).try_despawn();
            }
            kept
        });
        // A parent no longer asked about is gone or empty.
        let parents: std::collections::HashSet<Entity> = wanted.iter().map(|(p, _)| *p).collect();
        self.orders.retain(|p, _| parents.contains(p));

        for (parent, items) in wanted {
            let mut order = Vec::with_capacity(items.len());
            for (key, model) in items {
                let entity = match self.entries.get_mut(key) {
                    Some((entity, last)) => {
                        if last != model {
                            update(commands, *entity, model);
                            *last = model.clone();
                        }
                        *entity
                    }
                    None => {
                        let entity = spawn(commands, key, model);
                        self.entries.insert(key.clone(), (entity, model.clone()));
                        entity
                    }
                };
                order.push(entity);
            }
            if self.orders.get(parent) != Some(&order) {
                commands.entity(*parent).replace_children(&order);
                self.orders.insert(*parent, order);
            }
        }
    }
}
