//! What a connector offers, and what the driver will build when asked.
//!
//! ```text
//! ovs-modes                          what every connector offers
//! ovs-modes 1920x1080@60             and whether the driver will build that
//! ovs-modes 1920x1080@59.94 1280x720@50
//! ```
//!
//! **It needs the display to itself**, so the renderer has to be stopped while
//! it runs: `sudo systemctl stop ovs-renderer`, then this, then start it again.
//! A wall goes dark for as long as that takes.
//!
//! # Why this exists
//!
//! "The show decides the mode, whatever the EDID says" is the owner's
//! requirement (2026-09-25), and the answer to whether it can be met is the
//! driver's, not this codebase's. `vkCreateDisplayModeKHR` is the whole of the
//! request; when it refuses there is nothing above it to try harder with.
//!
//! So this asks, and prints what came back. Which turns "the force is broken"
//! into one of two very different statements: *this driver never builds a mode
//! it did not enumerate*, or *this driver would have built it and something
//! here asked wrongly*. Guessing between those cost a day.
//!
//! Every existing mode is asked for as well as the ones named on the command
//! line. A driver that refuses even a mode it just listed is a driver whose
//! refusal says nothing about the mode — and that is worth knowing before
//! anybody reads anything into the refusals that matter.

use std::process::ExitCode;

use ovs_renderer::vulkan::VulkanContext;
use ovs_types::geometry::{FrameRate, SizePx};

fn main() -> ExitCode {
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    if wanted
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        println!("usage: ovs-modes [WIDTHxHEIGHT@HZ ...]");
        return ExitCode::SUCCESS;
    }

    let asked: Vec<(SizePx, FrameRate)> = match wanted.iter().map(|text| parse(text)).collect() {
        Ok(asked) => asked,
        Err(reason) => {
            eprintln!("{reason}");
            return ExitCode::FAILURE;
        }
    };

    let context = match VulkanContext::open("ovs-modes", None) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("cannot open a GPU: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !context.has_display() {
        eprintln!("this machine's Vulkan has no display extensions");
        return ExitCode::FAILURE;
    }

    let displays = match context.displays() {
        Ok(displays) => displays,
        Err(error) => {
            eprintln!("cannot enumerate displays: {error}");
            return ExitCode::FAILURE;
        }
    };
    if displays.is_empty() {
        println!("this GPU is driving no connectors");
        return ExitCode::SUCCESS;
    }

    for display in &displays {
        println!(
            "\n{} ({})",
            display.model.as_deref().unwrap_or("no model reported"),
            display.connector
        );
        if display.modes.is_empty() {
            println!("  offers nothing");
        }
        for mode in &display.modes {
            println!(
                "  offers {}x{} @ {:.3} Hz",
                mode.size.width,
                mode.size.height,
                f64::from(mode.refresh_millihertz) / 1000.0
            );
        }

        // Its own modes first. A driver that will not rebuild one it just
        // listed is refusing for a reason that has nothing to do with the
        // timings, and every refusal below then means nothing.
        println!("  --- will the driver build it? ---");
        for mode in display.modes.clone() {
            let rate = FrameRate::from_millihertz(mode.refresh_millihertz);
            match rate {
                Some(rate) => report(&context, display, mode.size, rate, "listed"),
                None => println!(
                    "  {}x{} @ {} mHz: unusable as a ratio",
                    mode.size.width, mode.size.height, mode.refresh_millihertz
                ),
            }
        }
        for (size, rate) in &asked {
            report(&context, display, *size, *rate, "asked for");
        }
    }

    ExitCode::SUCCESS
}

/// Asks for one mode and prints what the driver said.
fn report(
    context: &VulkanContext,
    display: &ovs_renderer::vulkan::display::Display,
    size: SizePx,
    rate: FrameRate,
    why: &str,
) {
    let already = display.mode_for(size, rate).is_some();
    match context.force_mode(display, size, rate) {
        Ok(_) => println!(
            "  {}x{} @ {:.3} Hz ({why}{}): built",
            size.width,
            size.height,
            rate.as_f64(),
            if already { ", already offered" } else { "" }
        ),
        Err(error) => println!(
            "  {}x{} @ {:.3} Hz ({why}{}): REFUSED — {error}",
            size.width,
            size.height,
            rate.as_f64(),
            if already { ", already offered" } else { "" }
        ),
    }
}

/// `1920x1080@59.94` into a size and a rate.
fn parse(text: &str) -> Result<(SizePx, FrameRate), String> {
    let (size, rate) = text
        .split_once('@')
        .ok_or_else(|| format!("{text}: expected WIDTHxHEIGHT@HZ"))?;
    let (width, height) = size
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{text}: expected WIDTHxHEIGHT@HZ"))?;
    let width: u32 = width
        .trim()
        .parse()
        .map_err(|_| format!("{text}: {width} is not a width"))?;
    let height: u32 = height
        .trim()
        .parse()
        .map_err(|_| format!("{text}: {height} is not a height"))?;
    let hertz: f64 = rate
        .trim()
        .replace(',', ".")
        .parse()
        .map_err(|_| format!("{text}: {rate} is not a number of hertz"))?;
    if hertz <= 0.0 {
        return Err(format!("{text}: a rate is more than zero"));
    }
    // Millihertz, because that is what a driver states and what the show
    // stores: 59.94 is 59940 and not 59.94.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let millihertz = (hertz * 1000.0).round() as u32;
    let rate = FrameRate::from_millihertz(millihertz)
        .ok_or_else(|| format!("{text}: that rate cannot be written as an exact ratio"))?;
    Ok((SizePx::new(width, height), rate))
}
