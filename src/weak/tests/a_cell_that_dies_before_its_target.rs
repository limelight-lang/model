//! A cell dying while its target lives has to leave the table, or
//! the target's row maps to freed memory and the next `create` hands
//! that memory out. rc-trace frees its white set raw, so its kind-5
//! arm is a second site owing the same unregister.



