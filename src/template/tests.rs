use super::*;
use crate::class::ClassBuilder;
use crate::intern::intern_str;
use crate::memory::arena::Arena;
use crate::refcount::{ll_release, ll_retain};
use crate::string::ll_string_new;

fn with_ctx<R>(f: impl FnOnce(*mut LLContext) -> R) -> R {
    let mut arena = Arena::new();
    let mut ctx = LLContext { arena: &mut arena };
    let r = f(&mut ctx);
    arena.reset(|_| {});
    r
}

/// One site's static half: `"id = $id, name = $name!"` is three parts
/// around two holes.
fn shape_of(parts: &[&str]) -> Box<TemplateShape> {
    let interned: Vec<*const LLString> = parts.iter().map(|p| intern_str(p)).collect();
    Box::new(TemplateShape {
        value_count: (parts.len() - 1) as u32,
        parts: Box::leak(interned.into_boxed_slice()).as_ptr(),
    })
}

mod the_instance_as_an_ordinary_entity;
mod the_string_the_flattening_allocates;
mod what_flattening_produces;
