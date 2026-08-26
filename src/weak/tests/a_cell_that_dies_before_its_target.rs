//! A cell dying while its target lives has to leave the table, or
//! the target's row maps to freed memory and the next `create` hands
//! that memory out. A collector that freed a condemned cell without
//! running dispose would owe the same unregister at a second site;
//! `rc-cycle` frees through the ordinary death path and owes none
//! (`rfc/model/gc/rc-cycle.md`, "Cycle teardown", step 6).



