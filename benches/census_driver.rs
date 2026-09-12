//! The hardware arm of `PLAN.md` S40.3: the loads of `ll_model::loads` driven
//! through the public ABI in a binary linked to the ordinary library, so that
//! `perf stat` counts the collector as it is built and not the test build's
//! per-dispatch assertion and counters.
//!
//! Built and run by hand, never by the gate:
//!
//! ```text
//! cargo bench --no-run --features bench-loads --bench census_driver
//! mkfifo ctl ack
//! taskset -c 2 perf stat -D -1 --control=fifo:ctl,ack \
//!     -e instructions:u,cycles:u,L1-dcache-load-misses:u,dTLB-load-misses:u,branch-misses:u,cache-misses:u \
//!     -- target/release/deps/census_driver-<hash> dense:256:381 --control ctl ack
//! ```
//!
//! Without the feature the binary prints one line and exits, so that `cargo
//! bench --no-run` on the gate still compiles everything here but the two
//! feature-gated calls: the load's construction and the re-offer hook.
//!
//! # What the interval holds
//!
//! Counting is enabled after the workspace draw and one warm-up collection,
//! and disabled after the last measured collection. Inside the interval each
//! iteration is the poll (`ll_gc_maybe_collect`, which refills the queue's
//! spare cells as every safepoint does and fires nothing, the thread being
//! unarmed), the re-offer hook, and the collection. The poll and the hook are
//! inside because the collection needs them: a root read live is deferred at
//! the close, and without the refill the lane cannot take it. An empty
//! interval — `--collections 0` — bounds what the bracket itself costs.
//!
//! The manager's draws are read through `gc_metadata::stats` before and after
//! the interval: the peak is what the collections took past the workspace.

// Without the feature `run` reads none of this, and the warning would name
// every field the real driver reads.
#![cfg_attr(not(feature = "bench-loads"), allow(dead_code))]

use std::io::{BufRead, BufReader, Write};

/// One load, named on the command line.
///
/// `dense:<class>:<n>`, `sparse:<class>:<n>` (one member per block),
/// `group:<class>:<n>:<fillers>`, `edge2:<class>:<n>:<fillers>`,
/// `full:<class>`, `retained:<class>:<n>`.
#[derive(Clone, Copy, Debug)]
struct Named {
    class_bytes: usize,
    members: usize,
    fillers: usize,
    retained: bool,
    second_edge: bool,
}

fn parse(name: &str) -> Named {
    let parts: Vec<&str> = name.split(':').collect();
    let number = |index: usize| -> usize {
        parts
            .get(index)
            .and_then(|part| part.parse().ok())
            .unwrap_or_else(|| panic!("{name}: a number is missing at position {index}"))
    };
    let slots = |class_bytes: usize| ll_model::memory::block_pool::BLOCK_PAYLOAD / class_bytes;
    match parts[0] {
        "dense" => Named {
            class_bytes: number(1),
            members: number(2),
            fillers: 0,
            retained: false,
            second_edge: false,
        },
        "sparse" => Named {
            class_bytes: number(1),
            members: number(2),
            fillers: slots(number(1)) - 1,
            retained: false,
            second_edge: false,
        },
        "group" => Named {
            class_bytes: number(1),
            members: number(2),
            fillers: number(3),
            retained: false,
            second_edge: false,
        },
        "edge2" => Named {
            class_bytes: number(1),
            members: number(2),
            fillers: number(3),
            retained: false,
            second_edge: true,
        },
        "full" => Named {
            class_bytes: number(1),
            members: slots(number(1)),
            fillers: 0,
            retained: false,
            second_edge: false,
        },
        "retained" => Named {
            class_bytes: number(1),
            members: number(2),
            fillers: 0,
            retained: true,
            second_edge: false,
        },
        other => panic!("{other}: not a load"),
    }
}

/// The `perf stat --control` pair: a command written to the control fifo
/// and its acknowledgement read from the other, so that the interval starts
/// and ends where this process says.
struct Control {
    control: std::fs::File,
    ack: BufReader<std::fs::File>,
}

impl Control {
    fn open(control: &str, ack: &str) -> Self {
        Self {
            control: std::fs::OpenOptions::new()
                .write(true)
                .open(control)
                .expect("the control fifo opens"),
            ack: BufReader::new(std::fs::File::open(ack).expect("the ack fifo opens")),
        }
    }

    fn send(&mut self, command: &str) {
        writeln!(self.control, "{command}").expect("the command is written");
        self.control.flush().expect("and flushed");
        // perf writes `ack\n\0`, so the NUL of one acknowledgement opens the
        // next line.
        let mut line = String::new();
        self.ack.read_line(&mut line).expect("perf acknowledges");
        assert_eq!(
            line.trim_matches(|c: char| c == '\0' || c.is_whitespace()),
            "ack",
            "perf acknowledged {command:?} with {line:?}"
        );
    }
}

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    let Some(name) = arguments.get(1) else {
        eprintln!("usage: census_driver <load> [--collections N] [--control <ctl> <ack>]");
        std::process::exit(2);
    };
    let named = parse(name);
    let mut collections = 8;
    let mut control = None;
    let mut index = 2;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--collections" => {
                collections = arguments[index + 1].parse().expect("a count");
                index += 2;
            }
            "--control" => {
                control = Some(Control::open(&arguments[index + 1], &arguments[index + 2]));
                index += 3;
            }
            other => panic!("{other}: not an option"),
        }
    }

    run(named, collections, control);
}

#[cfg(not(feature = "bench-loads"))]
fn run(named: Named, _collections: usize, _control: Option<Control>) {
    println!("census_driver: built without `bench-loads`, nothing to drive for {named:?}");
}

#[cfg(feature = "bench-loads")]
fn run(named: Named, collections: usize, mut control: Option<Control>) {
    use ll_model::gc::{ll_gc_collect_cycles, ll_gc_maybe_collect, ll_gc_reoffer_deferred};
    use ll_model::loads::{self, Load, Population};
    use ll_model::memory::gc_metadata;

    assert!(
        ll_model::memory::heap::ll_thread_init(),
        "the pool served this thread"
    );
    let load = Load {
        class_bytes: named.class_bytes,
        members: named.members,
        fillers: named.fillers,
        population: if named.retained {
            Population::Retained
        } else {
            Population::Ordinary
        },
        second_edge: named.second_edge,
    };
    let roots = if named.retained { 1 } else { named.members };
    // The epoch turns over every 64 commits and a turnover's re-offer would
    // land inside the interval: one process stays under it by count.
    assert!(
        collections + 3 < 64,
        "the warm-up, the interval and the teardown stay inside one epoch"
    );
    let mut built = unsafe { loads::build(load) };

    // The poll once, outside the interval: it refills the spare cells the
    // registration may have drawn, and fires the collection the growth may
    // have armed.
    let polled = unsafe { ll_gc_maybe_collect() };
    assert_eq!(
        polled, 0,
        "the ring is live, so an armed poll frees nothing"
    );

    // The warm-up: the workspace draw, and the first trace over the ring.
    let moved = ll_gc_reoffer_deferred();
    assert_eq!(
        unsafe { ll_gc_collect_cycles() },
        0,
        "the keeper holds the ring"
    );
    let before = gc_metadata::stats();

    if let Some(control) = control.as_mut() {
        control.send("enable");
    }

    for collection in 1..=collections {
        assert_eq!(
            unsafe { ll_gc_maybe_collect() },
            0,
            "collection {collection}: the poll fires nothing"
        );
        let moved = ll_gc_reoffer_deferred();
        assert_eq!(
            moved, roots,
            "collection {collection}: every root read live was deferred"
        );
        assert_eq!(
            unsafe { ll_gc_collect_cycles() },
            0,
            "collection {collection}: the keeper holds the ring"
        );
    }

    if let Some(control) = control.as_mut() {
        control.send("disable");
    }

    let after = gc_metadata::stats();
    println!(
        "census_driver {name:?}: {collections} collections over {roots} roots; warm-up re-offered {moved}; \
         GC blocks current {} peak {} bytes in use current {} peak {} (before: current {} peak {})",
        after.current_blocks(),
        after.peak_blocks(),
        after.current_bytes_in_use(),
        after.peak_bytes_in_use(),
        before.current_blocks(),
        before.peak_blocks(),
        name = std::env::args().nth(1).unwrap_or_default(),
    );

    unsafe { loads::release_ring(&mut built) };
    let _ = ll_gc_reoffer_deferred();
    let freed = unsafe { ll_gc_collect_cycles() };
    if named.retained {
        // The members matured under the collections above and none is a
        // candidate, so the trace from the first member, registered by the
        // keeper's null store, stops at the second: the ring reads live and
        // waits for the turnover, which one process never reaches. That is
        // the recall the prune costs; the census arm reads the pruned edge
        // itself, this driver only the count (`cycle::loads::release_ring`).
        assert_eq!(freed, 0, "a mature retained ring waits for the turnover");
    } else {
        assert_eq!(freed, named.members, "the ring went with the keeper's edge");
    }
}
