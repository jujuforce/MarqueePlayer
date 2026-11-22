//! Path geometry shared by Slint's SVG commands and the native presenter.
use std::fmt::Write as _;

/// C layout: a 32-bit tag followed by up to six floats, all in logical pixels.
/// Circles are a closed counterclockwise subpath, including the hole in the ring's corona.
#[derive(Clone, Copy, Debug, PartialEq)]
#[repr(C, u32)]
pub enum PathCommand {
    Move(f32, f32) = 0,
    Line(f32, f32) = 1,
    Cubic(f32, f32, f32, f32, f32, f32) = 2,
    Close = 3,
    Circle(f32, f32, f32) = 4,
    Rect(f32, f32, f32, f32) = 5,
    /// A moving particle's stable ID lets the compositor follow it between audio samples.
    Particle(f32, f32, f32, u32) = 6,
}

const _: () = assert!(std::mem::size_of::<PathCommand>() == 7 * 4);

pub(super) trait PathSink {
    fn clear(&mut self);
    fn add(&mut self, command: PathCommand);
    fn append(&mut self, other: &Self);
}

impl PathSink for String {
    fn clear(&mut self) { String::clear(self); }

    fn append(&mut self, other: &Self) { self.push_str(other); }

    fn add(&mut self, command: PathCommand) {
        use PathCommand::*;
        match command {
            Move(x, y) => { let _ = write!(self, "M{x:.1} {y:.1}"); }
            Line(x, y) => { let _ = write!(self, "L{x:.1} {y:.1}"); }
            Cubic(a, b, c, d, x, y) => { let _ = write!(self, "C{a:.1} {b:.1} {c:.1} {d:.1} {x:.1} {y:.1}"); }
            Close => self.push('Z'),
            Circle(x, y, r) | Particle(x, y, r, _) => {
                // The two semicircles have identical arc parameters and return to the first point.
                let left = self.len() + 1;
                let _ = write!(self, "M{:.1} {y:.1}", x - r);
                let arc = self.len();
                let _ = write!(self, "A{r:.1} {r:.1} 0 1 0 ");
                let right = self.len();
                let _ = write!(self, "{:.1} {y:.1}", x + r);
                self.extend_from_within(arc..right);
                self.extend_from_within(left..arc);
                self.push('Z');
            }
            Rect(x, y, w, h) => { let _ = write!(self, "M{x:.1} {y:.1}H{:.1}V{:.1}H{x:.1}Z", x + w, y + h); }
        }
    }
}

#[cfg(any(test, target_os = "macos"))]
impl PathSink for Vec<PathCommand> {
    fn clear(&mut self) { Vec::clear(self); }
    fn add(&mut self, command: PathCommand) { self.push(command); }
    fn append(&mut self, other: &Self) { self.extend_from_slice(other); }
}
