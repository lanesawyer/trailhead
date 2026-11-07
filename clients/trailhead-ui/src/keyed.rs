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

    /// Despawns every entity (and what is under them) and forgets them, for when the whole surface changes subject.
    pub fn clear(&mut self, commands: &mut Commands) {
        for (entity, _) in self.entries.values() {
            commands.entity(*entity).try_despawn();
        }
        self.forget();
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

        let mut orders = Vec::with_capacity(wanted.len());
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
            orders.push((*parent, order));
        }
        // Detach what leaves a parent before any parent is given its new children. `replace_children` adds a child that
        // belongs elsewhere without updating its old parent's list, so if the new parent came first, the old parent's
        // own `replace_children` would see the child still listed there and take it away from its new parent too,
        // leaving it parentless at the window's top left (TRAIL-1009). `detach_children` only touches children still
        // pointing at that parent.
        for (parent, order) in &orders {
            if let Some(old) = self.orders.get(parent) {
                let leaving: Vec<Entity> = old.iter().copied().filter(|e| !order.contains(e)).collect();
                if !leaving.is_empty() {
                    commands.entity(*parent).detach_children(&leaving);
                }
            }
        }
        for (parent, order) in orders {
            if self.orders.get(&parent) != Some(&order) {
                commands.entity(parent).replace_children(&order);
                self.orders.insert(parent, order);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brings `keyed` to `wanted` (key lists per parent, the key as the model) and applies the commands.
    fn sync(world: &mut World, keyed: &mut Keyed<String>, wanted: &[(Entity, &[&str])]) {
        let wanted: Vec<(Entity, Vec<(String, String)>)> = wanted
            .iter()
            .map(|(p, keys)| (*p, keys.iter().map(|k| (k.to_string(), k.to_string())).collect()))
            .collect();
        let mut queue = bevy::ecs::world::CommandQueue::default();
        let mut commands = Commands::new(&mut queue, world);
        keyed.sync(&mut commands, &wanted, |c, _, _| c.spawn_empty().id(), |_, _, _| {});
        queue.apply(world);
    }

    fn children(world: &World, parent: Entity) -> Vec<Entity> {
        world.get::<Children>(parent).map(|c| c.to_vec()).unwrap_or_default()
    }

    fn parent_of(world: &World, child: Entity) -> Option<Entity> {
        world.get::<ChildOf>(child).map(ChildOf::parent)
    }

    #[test]
    fn a_key_that_moves_to_an_earlier_parent_keeps_its_entity_and_a_parent() {
        let mut world = World::new();
        let (a, b) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mut keyed = Keyed::default();
        sync(&mut world, &mut keyed, &[(a, &["x", "y"]), (b, &["z"])]);
        let z = keyed.entity("z").unwrap();
        // `z` moves from the later parent to the earlier one, which is brought up to date first.
        sync(&mut world, &mut keyed, &[(a, &["x", "y", "z"]), (b, &[])]);
        assert_eq!(keyed.entity("z"), Some(z));
        assert_eq!(parent_of(&world, z), Some(a));
        assert_eq!(children(&world, a).len(), 3);
        assert!(children(&world, b).is_empty());
    }

    #[test]
    fn a_key_that_moves_to_a_later_parent_keeps_its_entity_and_a_parent() {
        let mut world = World::new();
        let (a, b) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mut keyed = Keyed::default();
        sync(&mut world, &mut keyed, &[(a, &["x", "y"]), (b, &["z"])]);
        let x = keyed.entity("x").unwrap();
        sync(&mut world, &mut keyed, &[(a, &["y"]), (b, &["x", "z"])]);
        assert_eq!(parent_of(&world, x), Some(b));
        assert_eq!(children(&world, b), [x, keyed.entity("z").unwrap()]);
    }

    #[test]
    fn two_keys_that_swap_parents_both_land() {
        let mut world = World::new();
        let (a, b) = (world.spawn_empty().id(), world.spawn_empty().id());
        let mut keyed = Keyed::default();
        sync(&mut world, &mut keyed, &[(a, &["x"]), (b, &["y"])]);
        let (x, y) = (keyed.entity("x").unwrap(), keyed.entity("y").unwrap());
        sync(&mut world, &mut keyed, &[(a, &["y"]), (b, &["x"])]);
        assert_eq!((parent_of(&world, x), parent_of(&world, y)), (Some(b), Some(a)));
        assert_eq!((children(&world, a), children(&world, b)), (vec![y], vec![x]));
    }

    #[test]
    fn a_reorder_within_a_parent_keeps_every_entity() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let mut keyed = Keyed::default();
        sync(&mut world, &mut keyed, &[(a, &["x", "y", "z"])]);
        let before = children(&world, a);
        sync(&mut world, &mut keyed, &[(a, &["z", "x", "y"])]);
        assert_eq!(children(&world, a), [before[2], before[0], before[1]]);
    }

    #[test]
    fn a_key_no_longer_wanted_is_despawned() {
        let mut world = World::new();
        let a = world.spawn_empty().id();
        let mut keyed = Keyed::default();
        sync(&mut world, &mut keyed, &[(a, &["x", "y"])]);
        let y = keyed.entity("y").unwrap();
        sync(&mut world, &mut keyed, &[(a, &["x"])]);
        assert!(world.get_entity(y).is_err());
        assert_eq!(keyed.len(), 1);
    }
}
