//! The wire between the System screen and the renderer that shows it.
//!
//! One socket, and each direction carries one kind of thing, so neither needs a
//! tag to say which:
//!
//! * **console → renderer**: painted frames, and nothing else.
//! * **renderer → console**: how big the panel is, and what somebody did on it.
//!
//! # Why this is its own crate, with nothing in it
//!
//! It would have belonged in `ovs-ipc`, beside the control channel the core and
//! the renderer already share. It cannot be there, and the reason is worth
//! writing down because it will look like an arbitrary split otherwise.
//!
//! The console hosts CEF. Linking `getrandom` into a binary that also hosts CEF
//! makes Chromium abort during `CefInitialize`, inside
//! `ContentMainRunnerImpl::Initialize`, closing a file descriptor that will not
//! close. Established by bisection on the reference machine on 2026-09-26:
//! `serde_json` alone is harmless, `ovs-types` is not, `ulid` is not, and
//! `getrandom` alone reproduces it five times out of five. `ovs-ipc` reaches
//! `getrandom` through `ovs-types` and `ulid`.
//!
//! So this crate depends on nothing at all, and must go on depending on nothing.
//! Anything added here is added to the console's process, and the console's
//! process is one where an ordinary crate can stop Chromium from starting.
//!
//! # Pixels do not go through JSON
//!
//! Which is also why control is binary here rather than serialised. A frame at
//! 1024x600 is two and a half megabytes and could never have gone through JSON;
//! the two numbers beside it could have, and a serialiser is exactly the kind of
//! dependency this crate cannot have.

use std::io::{Read, Write};

/// The largest side a frame may claim, in pixels.
///
/// A reader trusts the header enough to allocate from it, so the header has to
/// be bounded by something. 7680 is a 16K raster's long side: beyond any panel
/// this will be asked to draw, and small enough that a wrong number cannot ask
/// for a machine's memory.
pub const MAX_SIDE: u32 = 7680;

/// Why a message could not be carried.
#[derive(Debug)]
pub enum WireError {
    /// The other end has gone.
    Closed,
    /// The socket failed.
    Io(std::io::Error),
    /// A message this cannot read, or one this would not write.
    Malformed(String),
}

impl std::fmt::Display for WireError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Closed => write!(formatter, "the other end has gone"),
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Malformed(why) => write!(formatter, "{why}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<std::io::Error> for WireError {
    fn from(error: std::io::Error) -> Self {
        if gone(error.kind()) {
            Self::Closed
        } else {
            Self::Io(error)
        }
    }
}

/// Whether this is the other end having gone rather than something wrong.
///
/// **All four, not just the end of the file.** A reader whose peer closed gets
/// `UnexpectedEof`; a *writer* gets a broken pipe, or a reset, or an abort,
/// depending on how far the peer got. Calling those faults means every ordinary
/// renderer restart leaves a warning in the log about a frame that could not be
/// sent — which is what this looked like the first time it was run against a
/// process that exited normally.
const fn gone(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
    )
}

/// What the renderer tells the console.
///
/// # Why the renderer sends the input, when the console is the one that wants it
///
/// The console could read the devices itself and save a hop. Two reasons it does
/// not. It hosts CEF, and every crate linked into that process is a gamble — one
/// ordinary dependency already stopped Chromium starting, which is why this crate
/// has none. And "only on the System Display" is a rule about *which panel*, so
/// it belongs in the process that owns the panels and knows which one this is.
///
/// Coordinates are pixels of that panel. The renderer set the mode and the
/// console was laid out for it, so both sides mean the same pixel by the same
/// number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToConsole {
    /// Lay the page out at this size from now on.
    ///
    /// The output's own mode, so the page is drawn for the panel it will be on
    /// rather than for a size written down somewhere. This is what makes the
    /// System screen responsive to the socket it lands on (owner, 2026-09-26:
    /// "es kann jede beliebige größe haben, deshalb sagte ich sollte es
    /// responsiv sein alles").
    Resize {
        /// Width in pixels.
        width: u32,
        /// Height in pixels.
        height: u32,
    },
    /// The pointer is here now.
    Pointer {
        /// Pixels from the left of the panel.
        x: u32,
        /// Pixels from the top of the panel.
        y: u32,
    },
    /// The pointer went down or came up, here.
    ///
    /// One button, because the System screen has one: it is a panel in a rack
    /// that people press with a finger, and there is no second button on a
    /// finger. A touch is sent as this too — down where it landed, up where it
    /// left — which is what makes the page's own handlers fire without it
    /// needing to know which it was.
    Button {
        /// Pixels from the left of the panel.
        x: u32,
        /// Pixels from the top of the panel.
        y: u32,
        /// True for down, false for up.
        down: bool,
    },
    /// The wheel turned, or two fingers moved, here.
    Scroll {
        /// Pixels from the left of the panel.
        x: u32,
        /// Pixels from the top of the panel.
        y: u32,
        /// How far sideways, in pixels. Positive is right.
        dx: i32,
        /// How far down, in pixels. Positive is down.
        dy: i32,
    },
}

/// The tag for [`ToConsole::Resize`].
const RESIZE: u8 = 1;
/// The tag for [`ToConsole::Pointer`].
const POINTER: u8 = 2;
/// The tag for [`ToConsole::Button`].
const BUTTON: u8 = 3;
/// The tag for [`ToConsole::Scroll`].
const SCROLL: u8 = 4;

/// One painted picture of the page.
///
/// BGRA, top row first, `width * height * 4` bytes — what CEF hands over and
/// what a texture is made of, carried without a conversion at either end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The pixels.
    pub pixels: Vec<u8>,
}

impl Frame {
    /// How many bytes a frame of this size holds.
    #[must_use]
    pub const fn bytes_for(width: u32, height: u32) -> u64 {
        width as u64 * height as u64 * 4
    }
}

/// Writes one frame: two numbers and the pixels.
///
/// # Errors
///
/// [`WireError`] when the socket will not take it, or when the frame does not
/// describe itself — a length that is not its own size is a bug on this side and
/// must not become a reader that allocates from a wrong number.
pub fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<(), WireError> {
    if frame.pixels.len() as u64 != Frame::bytes_for(frame.width, frame.height) {
        return Err(WireError::Malformed(format!(
            "a {}x{} frame carries {} bytes, not {}",
            frame.width,
            frame.height,
            Frame::bytes_for(frame.width, frame.height),
            frame.pixels.len()
        )));
    }

    writer.write_all(&frame.width.to_be_bytes())?;
    writer.write_all(&frame.height.to_be_bytes())?;
    writer.write_all(&frame.pixels)?;
    writer.flush()?;
    Ok(())
}

/// Reads one frame.
///
/// # Errors
///
/// [`WireError::Closed`] when the other end has gone, and
/// [`WireError::Malformed`] for a header naming a size this will not allocate
/// for.
pub fn read_frame(reader: &mut impl Read) -> Result<Frame, WireError> {
    let width = read_u32(reader)?;
    let height = read_u32(reader)?;

    // Bounded before anything is allocated. The header comes off a socket, and a
    // reader that believed it could be asked for a machine's memory by one wrong
    // number.
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(WireError::Malformed(format!(
            "a frame cannot be {width}x{height}"
        )));
    }

    let mut pixels = vec![0_u8; Frame::bytes_for(width, height) as usize];
    reader.read_exact(&mut pixels)?;
    Ok(Frame {
        width,
        height,
        pixels,
    })
}

/// Writes one control message.
///
/// # Errors
///
/// [`WireError`] when the socket will not take it.
pub fn write_control(writer: &mut impl Write, message: ToConsole) -> Result<(), WireError> {
    match message {
        ToConsole::Resize { width, height } => {
            writer.write_all(&[RESIZE])?;
            writer.write_all(&width.to_be_bytes())?;
            writer.write_all(&height.to_be_bytes())?;
        }
        ToConsole::Pointer { x, y } => {
            writer.write_all(&[POINTER])?;
            writer.write_all(&x.to_be_bytes())?;
            writer.write_all(&y.to_be_bytes())?;
        }
        ToConsole::Button { x, y, down } => {
            writer.write_all(&[BUTTON])?;
            writer.write_all(&x.to_be_bytes())?;
            writer.write_all(&y.to_be_bytes())?;
            writer.write_all(&[u8::from(down)])?;
        }
        ToConsole::Scroll { x, y, dx, dy } => {
            writer.write_all(&[SCROLL])?;
            writer.write_all(&x.to_be_bytes())?;
            writer.write_all(&y.to_be_bytes())?;
            writer.write_all(&dx.to_be_bytes())?;
            writer.write_all(&dy.to_be_bytes())?;
        }
    }
    writer.flush()?;
    Ok(())
}

/// Reads one control message.
///
/// # Errors
///
/// [`WireError::Closed`] when the other end has gone, and
/// [`WireError::Malformed`] for a tag this build does not know — which is a
/// renderer newer than this console, and is said plainly rather than skipped.
pub fn read_control(reader: &mut impl Read) -> Result<ToConsole, WireError> {
    let mut tag = [0_u8; 1];
    reader.read_exact(&mut tag)?;

    match tag[0] {
        RESIZE => Ok(ToConsole::Resize {
            width: read_u32(reader)?,
            height: read_u32(reader)?,
        }),
        POINTER => Ok(ToConsole::Pointer {
            x: read_u32(reader)?,
            y: read_u32(reader)?,
        }),
        BUTTON => Ok(ToConsole::Button {
            x: read_u32(reader)?,
            y: read_u32(reader)?,
            down: read_u8(reader)? != 0,
        }),
        SCROLL => Ok(ToConsole::Scroll {
            x: read_u32(reader)?,
            y: read_u32(reader)?,
            dx: read_u32(reader)? as i32,
            dy: read_u32(reader)? as i32,
        }),
        unknown => Err(WireError::Malformed(format!(
            "this build does not know control message {unknown}"
        ))),
    }
}

fn read_u32(reader: &mut impl Read) -> Result<u32, WireError> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u8(reader: &mut impl Read) -> Result<u8, WireError> {
    let mut byte = [0_u8; 1];
    reader.read_exact(&mut byte)?;
    Ok(byte[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> Frame {
        Frame {
            width,
            height,
            pixels: vec![0x7f; Frame::bytes_for(width, height) as usize],
        }
    }

    #[test]
    fn a_frame_comes_back_as_it_went() {
        let sent = frame(4, 3);
        let mut wire = Vec::new();
        write_frame(&mut wire, &sent).expect("writes");

        // Two numbers and then the pixels, with nothing else between: the size on
        // the wire is what a reader on the other side of a slow link is waiting
        // for, and it should be possible to say what it is.
        assert_eq!(wire.len(), 8 + 4 * 4 * 3);

        assert_eq!(read_frame(&mut wire.as_slice()).expect("reads"), sent);
    }

    #[test]
    fn a_frame_that_does_not_describe_itself_is_not_sent() {
        // A length that disagrees with the size is a bug on this side. Caught
        // here rather than becoming a reader that allocates from a wrong number
        // and then blocks for ever waiting for bytes nobody will send.
        let wrong = Frame {
            width: 4,
            height: 3,
            pixels: vec![0; 7],
        };
        let mut wire = Vec::new();
        assert!(write_frame(&mut wire, &wrong).is_err());
        assert!(wire.is_empty(), "a bad frame was half-written");
    }

    #[test]
    fn a_size_no_panel_has_is_refused_before_anything_is_allocated() {
        for (width, height) in [
            (0, 600),
            (1024, 0),
            (MAX_SIDE + 1, 600),
            (1024, MAX_SIDE + 1),
        ] {
            let mut wire = Vec::new();
            wire.extend_from_slice(&width.to_be_bytes());
            wire.extend_from_slice(&height.to_be_bytes());
            // Deliberately no pixels behind it: a reader that got as far as
            // reading them would block here, which is the failure this guards
            // against.
            assert!(
                read_frame(&mut wire.as_slice()).is_err(),
                "{width}x{height} was accepted"
            );
        }
    }

    #[test]
    fn an_end_that_has_gone_reads_as_closed_rather_than_as_a_fault() {
        let mut nothing: &[u8] = &[];
        assert!(matches!(read_frame(&mut nothing), Err(WireError::Closed)));
        assert!(matches!(read_control(&mut nothing), Err(WireError::Closed)));
    }

    #[test]
    fn an_end_that_has_gone_writes_as_closed_too() {
        // The four ways a socket says the peer is not there any more. A renderer
        // that exits normally gives the *writer* a broken pipe, not an end of
        // file, and calling that a fault put a warning in the log on every
        // ordinary restart.
        for kind in [
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
        ] {
            let mut broken = Broken(kind);
            assert!(
                matches!(
                    write_frame(&mut broken, &frame(2, 2)),
                    Err(WireError::Closed)
                ),
                "{kind:?}"
            );
            assert!(
                matches!(
                    write_control(
                        &mut broken,
                        ToConsole::Resize {
                            width: 8,
                            height: 8
                        }
                    ),
                    Err(WireError::Closed)
                ),
                "{kind:?}"
            );
        }

        // And something that is genuinely wrong is still genuinely wrong.
        let mut full = Broken(std::io::ErrorKind::StorageFull);
        assert!(matches!(
            write_frame(&mut full, &frame(2, 2)),
            Err(WireError::Io(_))
        ));
    }

    /// A socket that fails every write the way a given kind of failure does.
    struct Broken(std::io::ErrorKind);

    impl Write for Broken {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.0, "as if the peer had gone"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::new(self.0, "as if the peer had gone"))
        }
    }

    #[test]
    fn a_resize_survives_the_trip() {
        let sent = ToConsole::Resize {
            width: 2560,
            height: 1440,
        };
        let mut wire = Vec::new();
        write_control(&mut wire, sent).expect("writes");
        assert_eq!(wire.len(), 9, "a tag and two numbers");
        assert_eq!(read_control(&mut wire.as_slice()).expect("reads"), sent);
    }

    #[test]
    fn everything_somebody_can_do_to_the_panel_survives_the_trip() {
        // Every variant, in one place, so a new one added without a way to read
        // it back fails here rather than on a panel.
        let messages = [
            ToConsole::Resize {
                width: 1024,
                height: 600,
            },
            ToConsole::Pointer { x: 17, y: 599 },
            ToConsole::Button {
                x: 0,
                y: 0,
                down: true,
            },
            ToConsole::Button {
                x: 1023,
                y: 599,
                down: false,
            },
            // Negative both ways, because a scroll is the one thing here that
            // has a direction and `u32` is what it travels in.
            ToConsole::Scroll {
                x: 5,
                y: 6,
                dx: -120,
                dy: 120,
            },
        ];

        for sent in messages {
            let mut wire = Vec::new();
            write_control(&mut wire, sent).expect("writes");
            assert_eq!(
                read_control(&mut wire.as_slice()).expect("reads"),
                sent,
                "{sent:?}"
            );
        }
    }

    #[test]
    fn a_message_this_build_does_not_know_is_said_rather_than_skipped() {
        // A renderer newer than this console. Skipping it would leave the reader
        // half way through a message it cannot measure, and everything after
        // would be read as garbage.
        let mut wire: &[u8] = &[99, 0, 0, 0, 1];
        assert!(matches!(
            read_control(&mut wire),
            Err(WireError::Malformed(_))
        ));
    }
}
