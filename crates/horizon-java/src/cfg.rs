//! A method's control-flow graph (docs/java-compat/compiler.md, step 1):
//! basic blocks, their edges (an exception edge goes from each block a
//! handler covers to the handler), the dominator tree and the natural
//! loops. The backends use it: the WASM backend to make nested blocks and
//! loops, the native backend to give Cranelift the blocks as they are.
use crate::{
    bytecode::{Instruction, Operand},
    classfile::Handler,
    types::successors,
};
use std::collections::{BTreeSet, HashMap};

#[derive(Clone, Debug)]
pub struct Block {
    /// Its instructions: `insns[start..end]`.
    pub start: usize,
    pub end: usize,
    /// The blocks control goes to next (normal edges).
    pub succs: Vec<usize>,
    /// The handlers of the try ranges it is in (exception edges).
    pub handlers: Vec<usize>,
    pub preds: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Cfg {
    pub blocks: Vec<Block>,
    /// The block of each instruction.
    pub block_of: Vec<usize>,
    /// The blocks in reverse postorder from the entry (unreachable ones are
    /// not in it).
    pub rpo: Vec<usize>,
    /// The immediate dominator of each block (the entry's is itself; an
    /// unreachable block's is `usize::MAX`).
    pub idom: Vec<usize>,
    /// The loops: each header and the blocks of its loop, header included.
    pub loops: Vec<(usize, BTreeSet<usize>)>,
    /// Whether every edge back to an earlier block goes to a block that
    /// dominates it: then nested blocks and loops express the graph exactly.
    pub reducible: bool,
}

impl Cfg {
    pub fn new(insns: &[Instruction], handlers: &[Handler]) -> Cfg {
        let n = insns.len();
        let at: HashMap<u32, usize> = insns.iter().enumerate().map(|(i, x)| (x.at, i)).collect();
        let pos = |pc: u16| at.get(&u32::from(pc)).copied().unwrap_or(n);
        // Leaders: the first instruction, every target, every instruction
        // after a jump or an end, and the ends of try ranges and handlers.
        let mut leaders = BTreeSet::from([0usize]);
        for (i, insn) in insns.iter().enumerate() {
            let ends = matches!(
                insn.operand,
                Operand::Target(_) | Operand::Table { .. } | Operand::Lookup { .. }
            ) || matches!(insn.opcode, 0xAC..=0xB1 | 0xBF);
            if ends {
                leaders.extend(successors(insns, i, &at));
                leaders.insert(i + 1);
            }
        }
        for h in handlers {
            leaders.extend([pos(h.start), pos(h.end), pos(h.handler)]);
        }
        leaders.retain(|&i| i < n);
        let starts: Vec<usize> = leaders.into_iter().collect();
        let mut block_of = vec![0; n];
        let mut blocks: Vec<Block> = starts
            .iter()
            .enumerate()
            .map(|(b, &s)| {
                let end = starts.get(b + 1).copied().unwrap_or(n);
                for x in &mut block_of[s..end] {
                    *x = b;
                }
                Block {
                    start: s,
                    end,
                    succs: Vec::new(),
                    handlers: Vec::new(),
                    preds: Vec::new(),
                }
            })
            .collect();
        for b in 0..blocks.len() {
            let last = blocks[b].end - 1;
            let mut succs: Vec<usize> = successors(insns, last, &at)
                .into_iter()
                .map(|i| block_of[i])
                .collect();
            succs.dedup();
            let mut hs: Vec<usize> = handlers
                .iter()
                .filter(|h| (pos(h.start)..pos(h.end)).contains(&blocks[b].start))
                .map(|h| pos(h.handler))
                .filter(|&i| i < n)
                .map(|i| block_of[i])
                .collect();
            hs.dedup();
            blocks[b].succs = succs;
            blocks[b].handlers = hs;
        }
        for b in 0..blocks.len() {
            let targets: Vec<usize> = blocks[b]
                .succs
                .iter()
                .chain(&blocks[b].handlers)
                .copied()
                .collect();
            for t in targets {
                if !blocks[t].preds.contains(&b) {
                    blocks[t].preds.push(b);
                }
            }
        }
        let rpo = reverse_postorder(&blocks);
        let idom = dominators(&blocks, &rpo);
        let (loops, reducible) = natural_loops(&blocks, &rpo, &idom);
        Cfg {
            blocks,
            block_of,
            rpo,
            idom,
            loops,
            reducible,
        }
    }

    /// Whether block `a` dominates block `b`.
    pub fn dominates(&self, a: usize, b: usize) -> bool {
        let mut x = b;
        loop {
            if x == a {
                return true;
            }
            let d = self.idom[x];
            if d == x || d == usize::MAX {
                return false;
            }
            x = d;
        }
    }
}

fn edges(b: &Block) -> impl Iterator<Item = &usize> {
    b.succs.iter().chain(&b.handlers)
}

fn reverse_postorder(blocks: &[Block]) -> Vec<usize> {
    let mut seen = vec![false; blocks.len()];
    let mut post = Vec::new();
    // An explicit stack: (block, next edge to look at).
    let mut stack = vec![(0usize, 0usize)];
    seen[0] = true;
    while let Some((b, i)) = stack.pop() {
        let next = edges(&blocks[b]).nth(i).copied();
        match next {
            Some(t) => {
                stack.push((b, i + 1));
                if !seen[t] {
                    seen[t] = true;
                    stack.push((t, 0));
                }
            }
            None => post.push(b),
        }
    }
    post.reverse();
    post
}

/// Cooper, Harvey and Kennedy's iterative dominator algorithm.
fn dominators(blocks: &[Block], rpo: &[usize]) -> Vec<usize> {
    let mut order = vec![usize::MAX; blocks.len()];
    for (i, &b) in rpo.iter().enumerate() {
        order[b] = i;
    }
    let mut idom = vec![usize::MAX; blocks.len()];
    idom[0] = 0;
    let intersect = |idom: &[usize], mut a: usize, mut b: usize| {
        while a != b {
            while order[a] > order[b] {
                a = idom[a];
            }
            while order[b] > order[a] {
                b = idom[b];
            }
        }
        a
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &b in rpo.iter().skip(1) {
            let mut new = usize::MAX;
            for &p in &blocks[b].preds {
                if idom[p] == usize::MAX {
                    continue;
                }
                new = if new == usize::MAX {
                    p
                } else {
                    intersect(&idom, p, new)
                };
            }
            if new != usize::MAX && idom[b] != new {
                idom[b] = new;
                changed = true;
            }
        }
    }
    idom
}

/// The natural loop of each back edge (merged by header), and whether
/// every retreating edge is a back edge.
fn natural_loops(
    blocks: &[Block],
    rpo: &[usize],
    idom: &[usize],
) -> (Vec<(usize, BTreeSet<usize>)>, bool) {
    let mut order = vec![usize::MAX; blocks.len()];
    for (i, &b) in rpo.iter().enumerate() {
        order[b] = i;
    }
    let dominates = |a: usize, b: usize| {
        let mut x = b;
        loop {
            if x == a {
                return true;
            }
            let d = idom[x];
            if d == x || d == usize::MAX {
                return false;
            }
            x = d;
        }
    };
    let mut reducible = true;
    let mut loops: Vec<(usize, BTreeSet<usize>)> = Vec::new();
    for &b in rpo {
        for &t in edges(&blocks[b]) {
            if order[t] > order[b] {
                continue;
            }
            // A retreating edge: a back edge only if its target dominates.
            if !dominates(t, b) {
                reducible = false;
                continue;
            }
            let body = match loops.iter_mut().find(|(h, _)| *h == t) {
                Some((_, body)) => body,
                None => {
                    loops.push((t, BTreeSet::from([t])));
                    &mut loops.last_mut().expect("just pushed").1
                }
            };
            let mut work = vec![b];
            while let Some(x) = work.pop() {
                if body.insert(x) {
                    work.extend(blocks[x].preds.iter().copied());
                }
            }
        }
    }
    (loops, reducible)
}
