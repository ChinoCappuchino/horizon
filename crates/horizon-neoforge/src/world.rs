//! Mods' block code in the game (docs/java-compat/plan.md, phase 0, step 4):
//! a running [`Session`] answers Horizon's [`BlockCode`] hook. When a player
//! uses a mod's block, the game calls the block's `useWithoutItem` in the
//! mod's own code, which acts on the level through [`CodeLevel`]; a block
//! that does not override it is left to its game class.
//!
//! The session runs on one thread (the server's): it is kept there in a
//! thread local, and the hook gives `None` on any other thread.
use crate::{host::World, objects::*, translate::Session};
use horizon_core::{
    geometry::{Direction, Vec3},
    position::BlockPos,
};
use horizon_java::runtime::Value;
use horizon_world::{
    level::{
        Behaviours, InteractionResult, PlayerContext,
        code::{BlockCode, CodeLevel},
    },
    registry::{BlockRegistry, BlockStateId},
};
use std::{cell::RefCell, collections::BTreeMap, sync::Arc};

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
}

struct Live {
    session: Session,
    registry: &'static BlockRegistry,
    blocks: Arc<BTreeMap<String, Value>>,
}

/// The hook: which states are mods' blocks with code.
pub struct JavaBlocks {
    has: Vec<bool>,
}

const USE: &str = "(Lnet/minecraft/world/level/block/state/BlockState;Lnet/minecraft/world/level/Level;Lnet/minecraft/core/BlockPos;Lnet/minecraft/world/entity/player/Player;Lnet/minecraft/world/phys/BlockHitResult;)Lnet/minecraft/world/InteractionResult;";

/// Keeps `session` on this thread and gives its blocks' code to the game.
pub fn install(session: Session, registry: &'static BlockRegistry, behaviours: &Behaviours) {
    let blocks: Arc<BTreeMap<String, Value>> = Arc::new(session.blocks.iter().cloned().collect());
    let has = (0..registry.len() as u32)
        .map(|i| {
            registry
                .state(BlockStateId(i))
                .is_some_and(|s| blocks.contains_key(&s.name))
        })
        .collect();
    LIVE.with(|l| {
        *l.borrow_mut() = Some(Live {
            session,
            registry,
            blocks,
        })
    });
    behaviours.set_code(Arc::new(JavaBlocks { has }));
}

/// Whether this thread holds a session.
pub fn running() -> bool {
    LIVE.with(|l| l.borrow().is_some())
}

fn direction_name(d: Direction) -> &'static str {
    match d {
        Direction::Down => "DOWN",
        Direction::Up => "UP",
        Direction::North => "NORTH",
        Direction::South => "SOUTH",
        Direction::West => "WEST",
        Direction::East => "EAST",
    }
}

impl BlockCode for JavaBlocks {
    fn has(&self, state: BlockStateId) -> bool {
        self.has.get(state.0 as usize).copied().unwrap_or(false)
    }

    fn use_without_item(
        &self,
        level: &mut dyn CodeLevel,
        pos: BlockPos,
        face: Direction,
        _hit: Vec3,
        player: &PlayerContext,
    ) -> Option<InteractionResult> {
        LIVE.with(|l| {
            // Already running (the code's own call made the game call in
            // again): the game's behaviour.
            let mut l = l.try_borrow_mut().ok()?;
            let live = l.as_mut()?;
            let host = live.session.host.clone();
            // SAFETY: the pointer is used only while this call runs, on
            // this thread, and cleared before it returns.
            let level: *mut (dyn CodeLevel + '_) = level;
            let level: *mut (dyn CodeLevel + 'static) = unsafe { std::mem::transmute(level) };
            *host.world.borrow_mut() = Some(World {
                level,
                registry: live.registry,
                blocks: live.blocks.clone(),
                using: pos,
            });
            let result = (|| {
                let id = unsafe { &*level }.block_state(pos);
                let state = host.java_state(id)?;
                let Some(Obj::State(def, _)) = get(&state) else {
                    return None;
                };
                let block = def.block.lock().unwrap().clone()?;
                let args = [
                    state.clone(),
                    obj(Obj::Level),
                    obj(Obj::Pos(pos.x, pos.y, pos.z)),
                    obj(Obj::Player {
                        can_eat: player.can_eat,
                        creative: player.game_master,
                    }),
                    obj(Obj::Hit(direction_name(face).into(), pos.x, pos.y, pos.z)),
                ];
                let mut cx = live.session.rt.context();
                match cx.call_virtual(&block, "useWithoutItem", USE, &args) {
                    Ok(Some(v)) => match get(&v) {
                        Some(Obj::Const(_, n)) => Some(match n.as_str() {
                            "SUCCESS" | "SUCCESS_NO_ITEM_USED" | "SUCCESS_SERVER" => {
                                InteractionResult::Success
                            }
                            "CONSUME" | "CONSUME_PARTIAL" => InteractionResult::Consume,
                            _ => InteractionResult::Pass,
                        }),
                        // Its game class's (`super`), or nothing the layer
                        // knows: the game's behaviour.
                        _ => None,
                    },
                    Ok(None) => None,
                    Err(e) => {
                        eprintln!("Horizon: a mod's block code failed: {e:?}");
                        None
                    }
                }
            })();
            *host.world.borrow_mut() = None;
            result
        })
    }
}
