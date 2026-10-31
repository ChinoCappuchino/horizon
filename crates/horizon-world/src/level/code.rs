//! Block behaviour from mods' own code (fork `NeuHorizon`,
//! docs/java-compat/README.md): a compiled NeoForge mod's block runs its
//! `useWithoutItem` (and, later, its other overrides) here, before the game
//! class's behaviour its template gives. The mod's code acts through
//! [`CodeLevel`]: the same block changes, sounds and player effects the
//! game's own behaviour makes.
use super::{InteractionResult, LevelAccess, PlayerContext, PlayerEffect, Sim};
use crate::registry::BlockStateId;
use horizon_core::{
    geometry::{Direction, Vec3},
    position::BlockPos,
};
use std::sync::Arc;

/// The game events a mod's block code may send.
const GAME_EVENTS: [&str; 10] = [
    "minecraft:eat",
    "minecraft:drink",
    "minecraft:block_change",
    "minecraft:block_place",
    "minecraft:block_destroy",
    "minecraft:block_open",
    "minecraft:block_close",
    "minecraft:block_activate",
    "minecraft:block_deactivate",
    "minecraft:item_interact_finish",
];

/// Mods' code for their blocks. It runs on the thread that holds it (the
/// server thread): elsewhere it gives `None`, and the game's behaviour runs.
pub trait BlockCode: Send + Sync {
    /// Whether code of a mod handles blocks in this state.
    fn has(&self, state: BlockStateId) -> bool;
    /// `useWithoutItem` of the block at `pos`: None leaves it to the game.
    fn use_without_item(
        &self,
        level: &mut dyn CodeLevel,
        pos: BlockPos,
        face: Direction,
        hit: Vec3,
        player: &PlayerContext,
    ) -> Option<InteractionResult>;
}

/// What a mod's code does to the world while it runs.
pub trait CodeLevel {
    fn block_state(&self, pos: BlockPos) -> BlockStateId;
    /// `Level.setBlock` with its flags.
    fn set_block(&mut self, pos: BlockPos, state: BlockStateId, flags: i32) -> bool;
    /// `Level.removeBlock`.
    fn remove_block(&mut self, pos: BlockPos) -> bool;
    fn player_effect(&mut self, pos: BlockPos, effect: PlayerEffect);
    /// `Level.playSound(null, pos, sound, ..., volume, pitch)`.
    fn sound(&mut self, pos: BlockPos, sound: &str, volume: f32, pitch: f32);
    fn game_event(&mut self, pos: BlockPos, event: &str);
}

impl<L: LevelAccess + ?Sized> CodeLevel for Sim<'_, L> {
    fn block_state(&self, pos: BlockPos) -> BlockStateId {
        self.state(pos)
    }
    fn set_block(&mut self, pos: BlockPos, state: BlockStateId, flags: i32) -> bool {
        Sim::set_block(self, pos, state, flags)
    }
    fn remove_block(&mut self, pos: BlockPos) -> bool {
        Sim::remove_block(self, pos, false)
    }
    fn player_effect(&mut self, pos: BlockPos, effect: PlayerEffect) {
        let state = self.state(pos);
        self.emit(super::LevelEvent::Player { pos, state, effect });
    }
    fn sound(&mut self, pos: BlockPos, sound: &str, volume: f32, pitch: f32) {
        let sound: Arc<str> = sound.trim_start_matches("minecraft:").into();
        Sim::sound(self, pos, &sound, volume, pitch);
    }
    fn game_event(&mut self, pos: BlockPos, event: &str) {
        // Game events are named by &'static str: the game's own.
        if let Some(e) = GAME_EVENTS.iter().find(|e| **e == event) {
            Sim::game_event(self, pos, e);
        }
    }
}
