use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sim::FixedTimestep;

const TICK_RATE_HZ: u32 = 60;
/// After a stall, run at most this many ticks to catch up and drop the rest.
const MAX_TICKS_PER_ADVANCE: u32 = 5;

fn main() {
    let running = Arc::new(AtomicBool::new(true));
    let handler_flag = Arc::clone(&running);
    ctrlc::set_handler(move || handler_flag.store(false, Ordering::SeqCst))
        .expect("failed to install Ctrl-C handler");

    let mut clock =
        FixedTimestep::new(Duration::from_secs(1) / TICK_RATE_HZ, MAX_TICKS_PER_ADVANCE);
    let mut tick: u64 = 0;
    let mut last = Instant::now();
    println!("server running at {TICK_RATE_HZ} Hz, Ctrl-C to stop");

    while running.load(Ordering::SeqCst) {
        std::thread::sleep(clock.until_next_tick());
        let now = Instant::now();
        for _ in 0..clock.advance(now - last) {
            tick += 1;
            // Logging every tick would flood the terminal; log once per simulated second.
            if tick.is_multiple_of(u64::from(TICK_RATE_HZ)) {
                println!("tick {tick}");
            }
        }
        last = now;
    }

    println!("shutting down after {tick} ticks");
}
