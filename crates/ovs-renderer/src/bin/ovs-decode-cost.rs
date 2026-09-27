//! What one decoder costs, and whether it gives it back.
//!
//! ```text
//! ovs-decode-cost clip.mov                    one player, a thousand frames
//! ovs-decode-cost clip.mov --players 3        three at once, as a show would
//! ovs-decode-cost clip.mov --frames 5000 --step 2 --rate 60
//! ovs-decode-cost clip.mov --churn 200     open and close one, two hundred times
//! ```
//!
//! # The churn mode
//!
//! What an operator does to a layer when they change their mind quickly: a
//! player opened, a few frames taken from it, the player dropped, and the next
//! one opened. Every one of those is a decoder — and on this machine a
//! hardware decoder, which holds memory *on the card* rather than in this
//! process.
//!
//! It exists because a renderer was found holding 7.9 GiB of an 8 GiB card
//! after an evening of exactly that, with the clip on the wall replaced by its
//! placeholder. Resident size is printed each round as everything
//! else here is; what the card is holding has to be read from outside, with
//! `nvidia-smi --query-compute-apps=pid,used_memory --format=csv`, because
//! Vulkan and CUDA both decline to say.
//!
//! # Why this exists
//!
//! A soak on the test machine measured the renderer holding about half a
//! gigabyte with one clip playing and about seven and a half with three — and
//! still holding five after two of them stopped. Neither number is explained by
//! the frame ring, which is four frames deep by construction, so the cost is
//! somewhere between the decoder and the allocator and neither one can be asked
//! through the API.
//!
//! This asks them directly. It opens real players on a real file, drives them
//! the way the render loop does, and reads this process's own resident size and
//! processor time at each step. Nothing here talks to a GPU, a server or a
//! display: the point is to be runnable on a machine that has none of them,
//! including the reference server, where the questions "how many layers fit in
//! this much memory" and "how many decoders fit in this many cores" both have to
//! be answered before a show is built on the answer.
//!
//! # The second question, which is now the important one
//!
//! The server plays what it is given, and the owner's target is seven
//! layers. Which codec a show is in therefore decides what the machine can hold,
//! and the two halves of that are printed here together: **cores**, which says
//! whether the decoders fit, and **frames held**, which says what it looks like
//! when they do not. A decoder that cannot keep up does not fail — it leaves the
//! pass holding the frame it had last, which on a wall is a clip that stutters.
//!
//! # What the numbers mean
//!
//! Resident size is what the kernel says this process is holding, which is not
//! the same as what it asked for. An allocator that has handed back a
//! gigabyte's worth of eight-megabyte blocks may keep every one of them for its
//! own reuse, and that shows here as memory that never comes back — which is
//! exactly the thing worth knowing, because the machine sees it the same way.

#[cfg(not(unix))]
fn main() {
    eprintln!("ovs-decode-cost needs a decoder, which is only built on unix");
    std::process::exit(2);
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    unix::run()
}

#[cfg(unix)]
mod unix {
    use std::process::ExitCode;
    use std::time::{Duration, Instant};

    use ovs_renderer::media::{FrameSource, Player, VideoSource};
    use ovs_types::id::MediaItemId;

    /// How long to wait after dropping the players before reading memory again.
    ///
    /// A player's worker thread notices it has been asked to stop the next time
    /// it looks, which is once per frame it is decoding. A moment is more than
    /// enough, and reading immediately would measure the threads rather than
    /// what is left after them.
    const SETTLE: Duration = Duration::from_millis(500);

    /// What to say when the arguments do not make sense.
    /// How many frames either side of a jump `--seeks` times.
    const AROUND_A_SEEK: u32 = 8;

    const USAGE: &str =
        "usage: ovs-decode-cost <clip> [--players N] [--frames N] [--step N] [--rate N] [--threads N] [--seeks N]";

    /// Prints what one clip is, before it is measured.
    ///
    /// Its own short-lived source rather than one of the players', so this
    /// happens before the measurement opens anything and cannot be mistaken for
    /// part of it. A clip that will not open at all is left to the code below to
    /// report properly.
    fn describe(path: &str, threads: usize) {
        let Ok(source) = VideoSource::open_with_threads(path, threads) else {
            return;
        };
        println!(
            "{} {:?}{}",
            source.decoder_name(),
            source.depth(),
            match source.conversion() {
                Some(conversion) => format!(", converting {conversion} every frame"),
                None => ", nothing converting".to_owned(),
            }
        );
    }

    pub fn run() -> ExitCode {
        let mut path = None;
        let mut players = 1usize;
        let mut frames = 1000u32;
        let mut step = 1u32;
        let mut rate = 60u32;
        let mut threads = 0usize;
        let mut seeks = 0u32;
        let mut churn = 0u32;

        let mut args = std::env::args().skip(1);
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--players" => players = parse(args.next().as_deref(), "--players"),
                "--frames" => frames = parse(args.next().as_deref(), "--frames"),
                "--step" => step = parse(args.next().as_deref(), "--step"),
                "--rate" => rate = parse(args.next().as_deref(), "--rate"),
                "--threads" => threads = parse(args.next().as_deref(), "--threads"),
                "--seeks" => seeks = parse(args.next().as_deref(), "--seeks"),
                "--churn" => churn = parse(args.next().as_deref(), "--churn"),
                "--help" | "-h" => {
                    println!("{USAGE}");
                    return ExitCode::SUCCESS;
                }
                other if other.starts_with('-') => {
                    eprintln!("unknown option {other}");
                    return ExitCode::from(2);
                }
                other => path = Some(other.to_owned()),
            }
        }

        let Some(path) = path else {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        };

        if seeks > 0 {
            return seek_cost(&path, threads, seeks);
        }

        println!("{path}");
        println!("{players} player(s), {frames} passes at {rate} a second, stepping by {step}");
        if threads == 0 {
            println!("decoding with the threads the crate chooses");
        } else {
            println!("decoding with {threads} thread(s) a clip");
        }
        // Said before anything is measured, because it changes what the numbers
        // below mean. A ten-bit clip moves twice the bytes a frame; a clip with
        // something converting it is paying for `swscale` on every pass and the
        // throughput measured is that, not the decoder's.
        describe(&path, threads);
        println!();
        report("before anything is open", None);
        let cpu_before_open = processor_seconds();

        if churn > 0 {
            return churn_players(&path, threads, churn, frames.min(60));
        }

        let mut open = Vec::with_capacity(players);
        for _ in 0..players {
            let source = match VideoSource::open_with_threads(&path, threads) {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("{error}");
                    return ExitCode::from(1);
                }
            };
            match Player::start(MediaItemId::new(), Box::new(source)) {
                Ok(player) => open.push(player),
                Err(error) => {
                    eprintln!("a player could not be started: {error}");
                    return ExitCode::from(1);
                }
            }
        }

        let length = open[0].length();
        println!(
            "the clip is {} frames, {:.1} seconds",
            length.frame_count,
            length.duration_ms as f64 / 1000.0
        );
        let opened = report("with the players open", None);

        // Driven the way the render loop drives them: every player is asked for
        // the frame its playhead is on, once per pass, and the pass does not
        // wait for a decoder that is not ready. Paced, because asking as fast as
        // this loop can ask measures the loop — a player answers from its ring
        // without blocking, so an unpaced run finishes in microseconds having
        // given the decoders no time to decode anything.
        let period = Duration::from_nanos(1_000_000_000 / u64::from(rate.max(1)));
        let began = Instant::now();
        let cpu_began = processor_seconds();
        let mut due = began;
        let mut index = 0u32;
        let mut late = 0u64;
        for _ in 0..frames {
            for player in &mut open {
                let _ = player.frame(index);
            }
            index = (index + step) % length.frame_count.max(1);
            due += period;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        for player in &open {
            late += player.report().late;
        }
        let elapsed = began.elapsed();
        let cpu = processor_seconds() - cpu_began;

        println!();
        println!(
            "{frames} passes in {:.2}s — {:.0} passes a second, {late} frames held",
            elapsed.as_secs_f64(),
            f64::from(frames) / elapsed.as_secs_f64()
        );
        // Cores rather than a percentage, because a percentage needs somebody to
        // remember how many cores the machine has and this number does not. One
        // core fully busy is 1.00 whatever it is measured on, and a machine with
        // six of them holds six.
        println!(
            "{:.2} core(s) of processor, {:.3} a player — opening cost {:.2}",
            cpu / elapsed.as_secs_f64(),
            cpu / elapsed.as_secs_f64() / players as f64,
            cpu_began - cpu_before_open
        );
        let played = report("after playing", Some(opened));
        let per = played.saturating_sub(opened) / players as u64;
        println!("  which is {} a player above opening", megabytes(per));

        drop(open);
        std::thread::sleep(SETTLE);
        let after = report("after the players are dropped", Some(opened));
        println!(
            "  {} of the {} is still held",
            megabytes(after.saturating_sub(opened)),
            megabytes(played.saturating_sub(opened))
        );

        ExitCode::SUCCESS
    }

    /// Says what one half of a slot change cost the thread that did it.
    ///
    /// Best, median and worst rather than a mean: a mean over a hundred triggers
    /// hides exactly the one an operator noticed. Frames as well as
    /// milliseconds, because dropped frames are the unit the fault is seen in.
    fn report_cost(what: &str, costs: &mut [Duration]) {
        costs.sort_unstable();
        let (Some(best), Some(worst)) = (costs.first(), costs.last()) else {
            return;
        };
        let median = costs[costs.len() / 2];
        println!(
            "{what} cost the thread {:.1} ms best, {:.1} ms median, {:.1} ms worst",
            best.as_secs_f64() * 1000.0,
            median.as_secs_f64() * 1000.0,
            worst.as_secs_f64() * 1000.0,
        );
        println!(
            "  which at 60 is {:.1} frames best, {:.1} median, {:.1} worst",
            best.as_secs_f64() * 60.0,
            median.as_secs_f64() * 60.0,
            worst.as_secs_f64() * 60.0,
        );
    }

    /// Times one source either side of a jump, which is what a loop point is.
    ///
    /// A clip that reaches its end and starts again asks its decoder for a
    /// frame a long way from the one it just produced, and that is a seek. The
    /// cost is invisible in a steady-state measurement and is most of what an
    /// operator sees: measured on the test machine, seven layers of ordinary
    /// 1080p60 H.264 held nothing at all for three hundred passes and held
    /// 1346 frames over twelve hundred — the difference being three loops.
    ///
    /// Sequential frames are timed too, because the number that matters is the
    /// ratio: a decoder that takes two milliseconds a frame and eighty to seek
    /// stalls a show for forty frames every time a clip goes round.
    fn seek_cost(path: &str, threads: usize, seeks: u32) -> ExitCode {
        let mut source = match VideoSource::open_with_threads(path, threads) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
        };

        let frames = source.length().frame_count.max(1);
        println!("{path}");
        println!("{} — {frames} frames", source.decoder_name());
        println!();

        let mut sequential = Vec::new();
        let mut after_a_seek = Vec::new();

        for round in 0..seeks {
            // A long way from wherever the last round finished, which is what
            // a loop point is and what a trigger is.
            let landing = (round * 97) % frames;
            let began = Instant::now();
            if source.frame(landing).is_err() {
                eprintln!("the clip would not give frame {landing}");
                return ExitCode::from(1);
            }
            after_a_seek.push(began.elapsed());

            for step in 1..=AROUND_A_SEEK {
                let began = Instant::now();
                let _ = source.frame((landing + step) % frames);
                sequential.push(began.elapsed());
            }
        }

        let average = |times: &[Duration]| {
            times.iter().sum::<Duration>().as_secs_f64() * 1000.0 / times.len().max(1) as f64
        };
        let worst = |times: &[Duration]| {
            times
                .iter()
                .max()
                .copied()
                .unwrap_or_default()
                .as_secs_f64()
                * 1000.0
        };

        println!(
            "the frame after a jump   {:6.1} ms average, {:6.1} ms worst",
            average(&after_a_seek),
            worst(&after_a_seek)
        );
        println!(
            "the ones after it        {:6.1} ms average, {:6.1} ms worst",
            average(&sequential),
            worst(&sequential)
        );
        println!();
        println!(
            "a jump costs {:.0} sequential frames' worth of time",
            average(&after_a_seek) / average(&sequential).max(0.0001)
        );
        ExitCode::SUCCESS
    }

    /// Opens and drops one player at a time, over and over.
    ///
    /// The shape of a fast slot change, with the compositor, the scene and the
    /// clock all absent — so that what grows, if anything grows, is the
    /// decoder and nothing else.
    ///
    /// Each round takes `frames` frames, which is what makes it a slot change
    /// rather than an open-and-close: a decoder that is never asked for a frame
    /// may never allocate its surfaces, and would prove nothing.
    fn churn_players(path: &str, threads: usize, rounds: u32, frames: u32) -> ExitCode {
        println!("opening and closing one player {rounds} times, {frames} frames each");
        println!("watch the card from outside:");
        println!("  nvidia-smi --query-compute-apps=pid,used_memory --format=csv");
        println!("  this process is {}", std::process::id());
        println!();

        // Every tenth round, so the figure is readable rather than a wall.
        let every = (rounds / 10).max(1);
        // What the render thread pays at a trigger. Wall clock, not processor
        // time: a hardware decoder open spends most of its time waiting on the
        // driver, which costs the thread its frame and this process no processor
        // time at all, so a figure in processor seconds says it is free.
        let mut opening: Vec<Duration> = Vec::with_capacity(rounds as usize);
        // And what it pays to let go of the outgoing one. Dropping a player
        // joins its decode thread, which waits for whatever frame that thread is
        // in the middle of.
        let mut closing: Vec<Duration> = Vec::with_capacity(rounds as usize);
        for round in 0..rounds {
            let began = Instant::now();
            let source = match VideoSource::open_with_threads(path, threads) {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("round {round}: {error}");
                    return ExitCode::from(1);
                }
            };
            let mut player = match Player::start(MediaItemId::new(), Box::new(source)) {
                Ok(player) => player,
                Err(error) => {
                    eprintln!("round {round}: a player could not be started: {error}");
                    return ExitCode::from(1);
                }
            };
            // Both halves together, because both are on the render thread at a
            // trigger and an operator cannot tell them apart.
            opening.push(began.elapsed());

            let length = player.length();
            for index in 0..frames {
                let _ = player.frame(index % length.frame_count.max(1));
                // At something like a frame rate, so the decoder is given time
                // to actually decode and allocate.
                std::thread::sleep(Duration::from_millis(4));
            }

            // Dropped here, explicitly, which is what a slot change does to the
            // player it is replacing.
            let closed = Instant::now();
            drop(player);
            closing.push(closed.elapsed());

            if round % every == 0 || round + 1 == rounds {
                report(&format!("after {} round(s)", round + 1), None);
            }
        }

        println!();
        report_cost("opening", &mut opening);
        report_cost("closing", &mut closing);
        println!("if the card's figure is where it started, the decoder gives it back");
        ExitCode::SUCCESS
    }

    /// Prints this process's resident size, and returns it in kilobytes.
    fn report(what: &str, against: Option<u64>) -> u64 {
        let now = resident_kilobytes();
        match against {
            Some(base) if now >= base => {
                println!(
                    "{what:<34} {:>10}  (+{})",
                    megabytes(now),
                    megabytes(now - base)
                );
            }
            Some(base) => {
                println!(
                    "{what:<34} {:>10}  (-{})",
                    megabytes(now),
                    megabytes(base - now)
                );
            }
            None => println!("{what:<34} {:>10}", megabytes(now)),
        }
        now
    }

    /// Processor time this whole process has used, in seconds.
    ///
    /// The fourteenth and fifteenth fields of `/proc/self/stat` are user and
    /// system ticks, summed over every thread — which is what is wanted here,
    /// because a decoder's work happens on threads this program never sees.
    ///
    /// The tick rate is not asked of the system: `sysconf(_SC_CLK_TCK)` is 100
    /// on every Linux this could run on, has been for as long as the field has
    /// existed, and is fixed in the kernel's ABI rather than tunable. Linking a
    /// C call to read a constant would be the more fragile choice.
    fn processor_seconds() -> f64 {
        const TICKS_A_SECOND: f64 = 100.0;
        let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
            return 0.0;
        };
        // The second field is the executable name in brackets and may itself
        // contain spaces, so the fields are counted from after the closing one.
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            return 0.0;
        };
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // `state` is now field 0, so `utime` is 11 and `stime` is 12.
        let ticks = |at: usize| {
            fields
                .get(at)
                .and_then(|f| f.parse::<f64>().ok())
                .unwrap_or(0.0)
        };
        (ticks(11) + ticks(12)) / TICKS_A_SECOND
    }

    /// This process's resident set, in kilobytes.
    ///
    /// Read from `/proc/self/statm`, whose second field is resident pages. The
    /// same number `ps` prints, so a figure from here and a figure an operator
    /// reads off the machine are the same figure.
    fn resident_kilobytes() -> u64 {
        let page = 4096u64;
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|line| {
                line.split_whitespace()
                    .nth(1)
                    .and_then(|pages| pages.parse::<u64>().ok())
            })
            .map_or(0, |pages| pages * page / 1024)
    }

    fn megabytes(kilobytes: u64) -> String {
        format!("{} MB", kilobytes / 1024)
    }

    fn parse<T: std::str::FromStr>(value: Option<&str>, what: &str) -> T {
        let Some(value) = value else {
            eprintln!("{what} needs a value");
            std::process::exit(2);
        };
        match value.parse() {
            Ok(parsed) => parsed,
            Err(_) => {
                eprintln!("{what} does not understand {value}");
                std::process::exit(2);
            }
        }
    }
}
