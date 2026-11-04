//! Where the time of a frame goes on the CPU thread: the rows the overlay shows, each with its
//! average and maximum, and the one line a slow frame writes to the log. Some rows are waiting
//! (for vertical sync), not work, and are named so. This is the bookkeeping only. The client times its
//! parts of the frame with [`CpuBreakdown::start`] and [`CpuBreakdown::finish`], which are
//! `Instant` reads and array writes, and the window [`Renderer`](crate::Renderer) reports its own
//! ([`RenderCpu`]).

use std::fmt::Write;
use std::time::{Duration, Instant};

use crate::timing::Series;

/// A frame longer than this (in milliseconds) is written to the log.
pub const SLOW_FRAME_MS: f64 = 33.0;
/// Most slow frames written to the log in a second: a run of them is one problem, and writing
/// them all would not help the frames.
pub const MAX_SLOW_LINES_PER_SECOND: u32 = 3;

/// What a row measures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Milliseconds. The rows that are parts of the frame add up to it with [`Row::Other`].
    Millis,
    /// A number of things.
    Count,
    /// A number of bytes.
    Bytes,
}

/// One row of the breakdown. A row has one value a frame: the sum of what was added to it since
/// the frame before ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// The character's ticks (and the camera between them), less the ground fill in them.
    SimTicks,
    /// Ticks run.
    Ticks,
    /// Making the ground around the character in those ticks, which is not in
    /// [`Row::SimTicks`].
    GroundFill,
    /// Bricks the ground generated in those ticks.
    GroundBricks,
    /// Running inputs again after the server and the prediction differed, the ground made
    /// whole at once included.
    Resim,
    /// Times that resimulation made a whole ball of ground at once.
    FullFills,
    /// Holding a dig, and the Edit it makes.
    Dig,
    /// Taking in the server's messages, less what resimulation took.
    Poll,
    /// Moving the rings to the camera.
    Recentre,
    /// Taking in the loader's bricks.
    Stream,
    /// Copying scene changes to the GPU. The row that asked for the copy does not count it.
    UpdateScene,
    /// Bytes those copies wrote.
    SceneBytes,
    /// Copies that made the buffers again and wrote all of the scene.
    Rebuilds,
    /// Finding what a click would change: the ghost.
    Aim,
    /// Laying out the overlay.
    Egui,
    /// Getting the frame to draw to from the surface. This is waiting more than work: where the
    /// present mode waits for vertical sync, it is here.
    Acquire,
    /// Encoding the passes, painting egui and submitting.
    Encode,
    /// Presenting the frame. Waiting as well, for some present modes.
    Present,
    /// The frame, less every part above.
    Other,
    /// The whole frame, from the end of the one before to its own end. This is the sum of the
    /// parts and [`Row::Other`], on their own clock: it is not `frame (CPU)` of the GPU rows, which
    /// is the time between two calls to [`Renderer::render`](crate::Renderer::render) and so
    /// leaves out what comes after the last of them and before the next.
    Frame,
}

impl Row {
    /// Every row, in the order the overlay shows them (and the order they are declared in).
    pub const ALL: [Row; 20] = [
        Row::SimTicks,
        Row::Ticks,
        Row::GroundFill,
        Row::GroundBricks,
        Row::Resim,
        Row::FullFills,
        Row::Dig,
        Row::Poll,
        Row::Recentre,
        Row::Stream,
        Row::UpdateScene,
        Row::SceneBytes,
        Row::Rebuilds,
        Row::Aim,
        Row::Egui,
        Row::Acquire,
        Row::Encode,
        Row::Present,
        Row::Other,
        Row::Frame,
    ];

    /// The row's label. Rows that say more of the row above are indented.
    pub fn name(self) -> &'static str {
        match self {
            Row::SimTicks => "sim ticks",
            Row::Ticks => "  ticks",
            Row::GroundFill => "ground fill",
            Row::GroundBricks => "  ground bricks",
            Row::Resim => "resim",
            Row::FullFills => "  full fills",
            Row::Dig => "dig",
            Row::Poll => "server poll",
            Row::Recentre => "recentre",
            Row::Stream => "stream",
            Row::UpdateScene => "update_scene",
            Row::SceneBytes => "  KiB written",
            Row::Rebuilds => "  rebuilds",
            Row::Aim => "ghost/aim",
            Row::Egui => "egui",
            Row::Acquire => "surface acquire (wait)",
            Row::Encode => "encode/submit",
            Row::Present => "present (wait)",
            Row::Other => "other",
            Row::Frame => "frame (end to end)",
        }
    }

    /// What the row's values are.
    pub fn unit(self) -> Unit {
        match self {
            Row::Ticks | Row::GroundBricks | Row::FullFills | Row::Rebuilds => Unit::Count,
            Row::SceneBytes => Unit::Bytes,
            Row::SimTicks
            | Row::GroundFill
            | Row::Resim
            | Row::Dig
            | Row::Poll
            | Row::Recentre
            | Row::Stream
            | Row::UpdateScene
            | Row::Aim
            | Row::Egui
            | Row::Acquire
            | Row::Encode
            | Row::Present
            | Row::Other
            | Row::Frame => Unit::Millis,
        }
    }

    /// Whether the row is a part of the frame that [`Row::Other`] is what is left of.
    fn is_part(self) -> bool {
        self.unit() == Unit::Millis && !matches!(self, Row::Other | Row::Frame)
    }

    fn index(self) -> usize {
        self as usize
    }

    /// `value` as the overlay writes it: milliseconds to the microsecond, counts to a tenth,
    /// bytes in KiB.
    pub fn format(self, value: f64) -> String {
        let unit = self.unit();
        format!("{:.*}", unit.decimals(true), unit.scaled(value))
    }

    /// `value` as the log writes it, with the row's name: what the overlay says, but coarser.
    fn describe(self, value: f64) -> String {
        let unit = self.unit();
        format!(
            "{} {:.*}{}",
            self.name().trim_start(),
            unit.decimals(false),
            unit.scaled(value),
            unit.suffix()
        )
    }
}

impl Unit {
    /// `value` in the unit that is shown: bytes are shown in KiB.
    fn scaled(self, value: f64) -> f64 {
        match self {
            Unit::Millis | Unit::Count => value,
            Unit::Bytes => value / 1024.0,
        }
    }

    /// Digits after the point, finer for the overlay's averages than for a line of the log.
    fn decimals(self, overlay: bool) -> usize {
        match (self, overlay) {
            (Unit::Millis, true) => 3,
            (Unit::Millis, false) => 1,
            (Unit::Count | Unit::Bytes, true) => 1,
            (Unit::Count | Unit::Bytes, false) => 0,
        }
    }

    /// Said after a value in the log.
    fn suffix(self) -> &'static str {
        match self {
            Unit::Millis => " ms",
            Unit::Count | Unit::Bytes => "",
        }
    }
}

const ROWS: usize = Row::ALL.len();

/// What the window renderer spent on the CPU in the frame it drew last, in milliseconds. All
/// zero for a frame it did not draw.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderCpu {
    /// Getting the surface's next frame, which can wait for vertical sync.
    pub acquire_ms: f64,
    /// From the frame being got to the commands being submitted: encoding, painting egui.
    pub encode_ms: f64,
    /// Presenting the frame.
    pub present_ms: f64,
}

/// The start of a timed part of the frame: see [`CpuBreakdown::start`].
#[derive(Clone, Copy, Debug)]
pub struct Span {
    at: Instant,
    /// What [`Row::UpdateScene`] had by then.
    uploaded_ms: f64,
}

/// Which slow frames get a line in the log.
#[derive(Clone, Debug, Default)]
struct SlowLog {
    /// When the second that is being counted began.
    since: Option<Instant>,
    written: u32,
    /// Slow frames not written since the last line, which the next line says.
    skipped: u32,
}

impl SlowLog {
    /// Whether a slow frame at `now` is written: [`MAX_SLOW_LINES_PER_SECOND`] are, in each
    /// second from the first. A second ends with the line that is the last it allows.
    fn allow(&mut self, now: Instant) -> bool {
        if self
            .since
            .is_none_or(|since| now.duration_since(since) >= Duration::from_secs(1))
        {
            self.since = Some(now);
            self.written = 0;
        }
        if self.written < MAX_SLOW_LINES_PER_SECOND {
            self.written += 1;
            true
        } else {
            self.skipped += 1;
            false
        }
    }
}

/// The rows of the breakdown, each over the last [`WINDOW`](crate::timing::WINDOW) frames.
#[derive(Clone, Debug)]
pub struct CpuBreakdown {
    /// What has been added to each row since the last frame ended.
    pending: [f64; ROWS],
    series: [Series; ROWS],
    /// When the last frame ended.
    last_end: Option<Instant>,
    slow: SlowLog,
}

impl Default for CpuBreakdown {
    fn default() -> Self {
        Self {
            pending: [0.0; ROWS],
            series: std::array::from_fn(|_| Series::new(false)),
            last_end: None,
            slow: SlowLog::default(),
        }
    }
}

impl CpuBreakdown {
    /// Adds `value` to what `row` has this frame.
    pub fn add(&mut self, row: Row, value: f64) {
        self.pending[row.index()] += value;
    }

    /// What `row` has so far this frame.
    pub fn pending(&self, row: Row) -> f64 {
        self.pending[row.index()]
    }

    /// Starts timing a part of the frame.
    pub fn start(&self) -> Span {
        Span {
            at: Instant::now(),
            uploaded_ms: self.pending(Row::UpdateScene),
        }
    }

    /// Adds the time since `span` started to `row`, less `less_ms` that another row was given in
    /// it, and less the scene copies made meanwhile (they are a row of their own).
    pub fn finish(&mut self, row: Row, span: Span, less_ms: f64) {
        let uploaded = self.pending(Row::UpdateScene) - span.uploaded_ms;
        self.add(
            row,
            span.at.elapsed().as_secs_f64() * 1e3 - uploaded - less_ms,
        );
    }

    /// The last [`WINDOW`](crate::timing::WINDOW) frames of `row`.
    pub fn series(&self, row: Row) -> &Series {
        &self.series[row.index()]
    }

    /// Ends a frame at `now`: its length is the time since the last one ended ([`Row::Frame`]),
    /// [`Row::Other`] is what none of the parts took, and every row records its value.
    /// Returns the line for the log if the frame was slow and the log is not at its rate limit.
    /// The first call only starts the clock.
    pub fn end_frame(&mut self, now: Instant) -> Option<String> {
        let Some(last) = self.last_end.replace(now) else {
            self.pending = [0.0; ROWS];
            return None;
        };
        let frame_ms = now.duration_since(last).as_secs_f64() * 1e3;
        let timed: f64 = Row::ALL
            .iter()
            .filter(|row| row.is_part())
            .map(|&row| self.pending(row))
            .sum();
        self.pending[Row::Other.index()] = frame_ms - timed;
        self.pending[Row::Frame.index()] = frame_ms;
        for (series, &value) in self.series.iter_mut().zip(&self.pending) {
            series.record(value);
        }
        let line =
            (frame_ms > SLOW_FRAME_MS && self.slow.allow(now)).then(|| self.slow_line(frame_ms));
        self.pending = [0.0; ROWS];
        line
    }

    /// The line for a slow frame: its length, every row that had something in it, and how many
    /// slow frames were left out since the last line.
    fn slow_line(&mut self, frame_ms: f64) -> String {
        let mut line = format!("slow frame {frame_ms:.1} ms:");
        let mut parts = Row::ALL
            .into_iter()
            .map(|row| (row, self.pending(row)))
            .filter(|&(row, value)| value != 0.0 && row != Row::Frame);
        if let Some((row, value)) = parts.next() {
            let _ = write!(line, " {}", row.describe(value));
        }
        for (row, value) in parts {
            let _ = write!(line, ", {}", row.describe(value));
        }
        let skipped = std::mem::take(&mut self.slow.skipped);
        if skipped > 0 {
            let _ = write!(
                line,
                " (+{skipped} slow frames not logged since the line before)"
            );
        }
        if self.slow.written == MAX_SLOW_LINES_PER_SECOND {
            let _ = write!(
                line,
                " (rate limit reached: more slow frames are not logged for up to 1 s)"
            );
        }
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// A breakdown whose clock has been started at `base`.
    fn started(base: Instant) -> CpuBreakdown {
        let mut cpu = CpuBreakdown::default();
        assert_eq!(cpu.end_frame(base), None);
        cpu
    }

    #[test]
    fn the_rows_are_in_the_order_they_are_declared() {
        for (i, row) in Row::ALL.into_iter().enumerate() {
            assert_eq!(row.index(), i, "{row:?}");
        }
    }

    #[test]
    fn a_row_has_the_sum_of_what_was_added_in_a_frame_and_other_is_what_is_left() {
        let base = Instant::now();
        let mut cpu = started(base);
        cpu.add(Row::SimTicks, 4.0);
        cpu.add(Row::SimTicks, 2.0);
        cpu.add(Row::Ticks, 3.0);
        cpu.add(Row::Stream, 1.0);
        cpu.add(Row::GroundFill, 0.5);
        cpu.end_frame(at(base, 10));
        assert_eq!(cpu.series(Row::SimTicks).last(), Some(6.0));
        assert_eq!(cpu.series(Row::Ticks).last(), Some(3.0));
        // A count is not time, so the frame's 10 ms is less 6 + 1 + 0.5 only, and the frame row
        // is not one of the parts it is less.
        assert!((cpu.series(Row::Other).last().unwrap() - 2.5).abs() < 1e-9);
        assert!((cpu.series(Row::Frame).last().unwrap() - 10.0).abs() < 1e-9);
        // The next frame starts from nothing.
        cpu.end_frame(at(base, 20));
        assert_eq!(cpu.series(Row::SimTicks).last(), Some(0.0));
        assert!((cpu.series(Row::Other).last().unwrap() - 10.0).abs() < 1e-9);
    }

    #[test]
    fn the_average_and_the_maximum_span_the_window_and_forget_what_is_older() {
        let base = Instant::now();
        let mut cpu = started(base);
        let mut now = 0;
        cpu.add(Row::Egui, 50.0);
        now += 60;
        cpu.end_frame(at(base, now));
        for _ in 0..crate::timing::WINDOW {
            cpu.add(Row::Egui, if now % 20 == 0 { 2.0 } else { 4.0 });
            now += 10;
            cpu.end_frame(at(base, now));
        }
        let egui = cpu.series(Row::Egui);
        // The 50 ms frame has left the window.
        assert_eq!(egui.max(), Some(4.0));
        let average = egui.average().unwrap();
        assert!((2.0..=4.0).contains(&average), "{average}");
    }

    #[test]
    fn the_first_frame_only_starts_the_clock_and_drops_what_was_added_before_it() {
        let base = Instant::now();
        let mut cpu = CpuBreakdown::default();
        cpu.add(Row::Egui, 9.0);
        assert_eq!(cpu.end_frame(base), None);
        assert_eq!(cpu.series(Row::Egui).average(), None);
        assert_eq!(cpu.pending(Row::Egui), 0.0);
    }

    #[test]
    fn a_timed_part_leaves_out_the_scene_copies_and_the_rows_made_inside_it() {
        let mut cpu = CpuBreakdown::default();
        let span = cpu.start();
        std::thread::sleep(Duration::from_millis(5));
        cpu.add(Row::UpdateScene, 2.0);
        cpu.finish(Row::Stream, span, 1.0);
        // At least the 5 ms slept, less the 3 that are left out: how much more depends on the
        // machine, so only the lower bound is checked.
        let took = cpu.pending(Row::Stream);
        assert!(took >= 1.99, "{took}");
    }

    #[test]
    fn a_frame_under_the_threshold_writes_nothing_and_a_slow_one_names_what_took_the_time() {
        let base = Instant::now();
        let mut cpu = started(base);
        cpu.add(Row::Egui, 1.0);
        assert_eq!(cpu.end_frame(at(base, 32)), None);
        cpu.add(Row::SimTicks, 30.5);
        cpu.add(Row::Ticks, 5.0);
        cpu.add(Row::SceneBytes, 2048.0);
        cpu.add(Row::Acquire, 10.0);
        let line = cpu.end_frame(at(base, 32 + 52)).unwrap();
        assert_eq!(
            line,
            "slow frame 52.0 ms: sim ticks 30.5 ms, ticks 5, KiB written 2, \
             surface acquire (wait) 10.0 ms, other 11.5 ms"
        );
        // Exactly the threshold is not over it.
        assert_eq!(cpu.end_frame(at(base, 32 + 52 + 33)), None);
    }

    #[test]
    fn the_log_takes_a_few_lines_a_second_says_how_many_it_left_out_and_starts_again() {
        let base = Instant::now();
        let mut cpu = started(base);
        let mut lines = Vec::new();
        // Ten slow frames of 40 ms: 400 ms, then four more seconds of them.
        let mut now = 0;
        for _ in 0..10 {
            now += 40;
            lines.extend(cpu.end_frame(at(base, now)));
        }
        assert_eq!(lines.len(), MAX_SLOW_LINES_PER_SECOND as usize);
        assert!(!lines[0].contains("not logged") && !lines[0].contains("rate limit"));
        // The line that uses up the second says so.
        assert!(
            lines[2]
                .ends_with("(rate limit reached: more slow frames are not logged for up to 1 s)")
        );
        // The next second begins with a line that counts what it missed.
        now = 1100;
        let line = cpu.end_frame(at(base, now)).unwrap();
        assert!(
            line.ends_with("(+7 slow frames not logged since the line before)"),
            "{line}"
        );
        // Frames that are not slow are not counted as missed.
        now += 10;
        assert_eq!(cpu.end_frame(at(base, now)), None);
    }

    #[test]
    fn the_rows_are_written_as_the_overlay_shows_them() {
        assert_eq!(Row::SimTicks.format(1.23456), "1.235");
        assert_eq!(Row::Ticks.format(2.34), "2.3");
        assert_eq!(Row::SceneBytes.format(3.0 * 1024.0), "3.0");
    }
}
