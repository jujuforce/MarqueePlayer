//! The visualizer's audio: a tap on the output thread ([`Tap`]) and the analysis that
//! turns what it holds into bar levels ([`Analyzer`]). No UI here.
//!
//! The tap takes the samples as they are written to the device, after ReplayGain and
//! before volume (so the picture does not shrink with the volume slider), mixed to mono,
//! into a ring of atomics: no allocation, no lock, nothing the output thread can wait
//! on, and when no one has turned it on, one relaxed load per write. The reader knows how
//! far behind the device is (what is queued in it plus its latency, published with the
//! position), so it reads the window that is being heard now, not the one just written.
//!
//! The analysis: a Hann-windowed FFT of the newest audible samples (8192 points at 44.1
//! and 48 kHz, more at higher rates, so the low bars are as fine at every rate), bars on a
//! logarithmic scale from 30 Hz to 16 kHz, each the loudest bin in its band (bands
//! narrower than a bin are measured at their centres), in dB (a full-scale sine reads 0 dB) tilted
//! +3 dB per octave around 1 kHz (music falls off about that fast, so without it the
//! treble would hardly move), mapped from `min_db..max_db` to 0..1. Each bar rises fast
//! and falls smoothly; a peak cap holds a moment above it, then drops.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// Mono frames the ring holds: 0.68 s at 192 kHz, enough for the largest FFT plus the
/// device's delay.
const RING: usize = 1 << 17;
const MASK: u64 = RING as u64 - 1;

/// The output thread's samples, for the visualizer. Get it from `Player::spectrum`.
pub struct Tap {
    active: AtomicBool,
    /// Allocated by the first `set_active(true)`, on the caller's thread.
    ring: OnceLock<Box<[AtomicU32]>>,
    /// Frames pushed, ever.
    written: AtomicU64,
    /// The frame being heard at `stamp`: `written` minus what the device holds and its
    /// latency.
    audible: AtomicU64,
    /// Nanoseconds since `epoch` when `audible` was published.
    stamp: AtomicU64,
    rate: AtomicU32,
    epoch: Instant,
}

impl Default for Tap {
    fn default() -> Self {
        Self {
            active: AtomicBool::new(false),
            ring: OnceLock::new(),
            written: AtomicU64::new(0),
            audible: AtomicU64::new(0),
            stamp: AtomicU64::new(0),
            rate: AtomicU32::new(48_000),
            epoch: Instant::now(),
        }
    }
}

impl Tap {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Start or stop taking samples. Off, the output thread does nothing for it.
    pub fn set_active(&self, on: bool) {
        if on && !self.active.load(Ordering::Acquire) {
            // Whatever the ring held from an earlier run is not what plays now: silence
            // until the new samples come (the output thread does not touch it while off).
            let ring = self.ring.get_or_init(|| (0..RING).map(|_| AtomicU32::new(0)).collect());
            for v in ring.iter() {
                v.store(0, Ordering::Relaxed);
            }
            let w = self.written.load(Ordering::Acquire);
            self.audible.store(w, Ordering::Relaxed);
            self.stamp.store(self.epoch.elapsed().as_nanos() as u64, Ordering::Release);
        }
        self.active.store(on, Ordering::Release);
    }

    /// The sample rate of the newest samples.
    pub fn rate(&self) -> u32 {
        self.rate.load(Ordering::Relaxed)
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Output thread: interleaved stereo, after gain and before volume.
    #[inline]
    pub(crate) fn push(&self, stereo: &[f32], rate: u32) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let Some(ring) = self.ring.get() else { return };
        let w = self.written.load(Ordering::Relaxed);
        let mut i = w;
        for f in stereo.as_chunks::<2>().0 {
            ring[(i & MASK) as usize].store(((f[0] + f[1]) * 0.5).to_bits(), Ordering::Relaxed);
            i += 1;
        }
        self.rate.store(rate, Ordering::Relaxed);
        self.written.store(i, Ordering::Release);
    }

    /// Output thread, with the position: the device holds `delay` frames not yet heard.
    #[inline]
    pub(crate) fn publish(&self, delay: u64) {
        if !self.active.load(Ordering::Relaxed) {
            return;
        }
        let w = self.written.load(Ordering::Relaxed);
        self.audible.store(w.saturating_sub(delay), Ordering::Relaxed);
        self.stamp.store(self.epoch.elapsed().as_nanos() as u64, Ordering::Release);
    }

    /// Fill `out` with the newest mono samples being heard now (oldest first), and say
    /// their sample rate. `running`: the device is playing, so the heard point moves on
    /// from where it was last published (at most to what was written).
    pub fn read(&self, out: &mut [f32], running: bool) -> u32 {
        let rate = self.rate.load(Ordering::Relaxed);
        let Some(ring) = self.ring.get() else {
            out.fill(0.0);
            return rate;
        };
        let stamp = self.stamp.load(Ordering::Acquire);
        let written = self.written.load(Ordering::Acquire);
        let mut end = self.audible.load(Ordering::Relaxed);
        if running {
            let since = (self.epoch.elapsed().as_nanos() as u64).saturating_sub(stamp);
            end = end.saturating_add(frames_in(since, rate));
        }
        let end = end.min(written);
        let n = out.len().min(RING) as u64;
        let start = end.saturating_sub(n);
        let pad = (n - (end - start)) as usize;
        out[..pad].fill(0.0);
        for (k, o) in out[pad..n as usize].iter_mut().enumerate() {
            *o = f32::from_bits(ring[((start + k as u64) & MASK) as usize].load(Ordering::Relaxed));
        }
        rate
    }
}

/// A real-input spectrum, computed in place with a half-length complex FFT.
struct Fft {
    n: usize,
    twiddles: Vec<(f32, f32)>,
    rev: Vec<u32>,
}

impl Fft {
    fn new(n: usize) -> Self {
        assert!(n.is_power_of_two() && n >= 4);
        let bits = (n / 2).trailing_zeros();
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = -2.0 * std::f64::consts::PI * k as f64 / n as f64;
                (a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let rev = (0..(n / 2) as u32).map(|i| i.reverse_bits() >> (32 - bits)).collect();
        Self { n, twiddles, rev }
    }

    /// Pack even and odd samples into one complex transform, then recover the positive
    /// frequency bins as interleaved real and imaginary components (Nyquist omitted).
    fn run(&self, samples: &mut [f32]) {
        debug_assert_eq!(samples.len(), self.n);
        let x = samples.as_chunks_mut::<2>().0;
        for (i, &j) in self.rev.iter().enumerate() {
            let j = j as usize;
            if j > i {
                x.swap(i, j);
            }
        }
        let mut len = 2;
        while len <= x.len() {
            let half = len / 2;
            let step = self.n / len;
            for group in x.chunks_exact_mut(len) {
                let (left, right) = group.split_at_mut(half);
                for (k, (a, b)) in left.iter_mut().zip(right).enumerate() {
                    let (wr, wi) = self.twiddles[k * step];
                    let tr = b[0] * wr - b[1] * wi;
                    let ti = b[0] * wi + b[1] * wr;
                    *b = [a[0] - tr, a[1] - ti];
                    *a = [a[0] + tr, a[1] + ti];
                }
            }
            len *= 2;
        }
        x[0] = [x[0][0] + x[0][1], 0.0];
        let middle = x.len() / 2;
        x[middle][1] = -x[middle][1]; // This bin is its own mirror.
        for k in 1..middle {
            let mirror = x.len() - k;
            let ([a, b], [c, d]) = (x[k], x[mirror]);
            // The even and odd sequences' spectra, split using their conjugate symmetry.
            let even = [(a + c) * 0.5, (b - d) * 0.5];
            let odd = [(b + d) * 0.5, (c - a) * 0.5];
            // The odd samples are one frame later than the even ones.
            let (wr, wi) = self.twiddles[k];
            let (r, i) = (wr * odd[0] - wi * odd[1], wr * odd[1] + wi * odd[0]);
            x[k] = [even[0] + r, even[1] + i];
            x[mirror] = [even[0] - r, i - even[1]];
        }
    }
}

/// How the bars read the spectrum and move.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub min_hz: f32,
    pub max_hz: f32,
    /// The bottom and top of the scale: below `min_db` a bar is flat, at `max_db` full.
    pub min_db: f32,
    pub max_db: f32,
    /// dB per octave added above 1 kHz (and taken off below).
    pub tilt_db: f32,
    /// Time constants (seconds) for a bar rising and falling.
    pub attack: f32,
    pub decay: f32,
    /// A peak cap stays this long, then falls with this acceleration (heights per s²).
    pub peak_hold: f32,
    pub peak_fall: f32,
    /// Follow the music's level (`update`): the scale slides, slowly and within -15 to
    /// +12 dB, so the loud bands of the last few seconds sit at about three quarters of
    /// the height. Loud masters no longer pin the bars to the top, and a quiet passage
    /// rises a little to keep moving. Off by default (the scale is fixed).
    pub auto_level: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { min_hz: 30.0, max_hz: 16_000.0, min_db: -72.0, max_db: -12.0, tilt_db: 3.0, attack: 0.025, decay: 0.22, peak_hold: 0.5, peak_fall: 2.5, auto_level: false }
    }
}

/// Where each bar reads the spectrum: a range of bins, or (narrower than a bin) a
/// fractional bin to measure directly.
#[derive(Clone, Copy, Debug)]
enum Band {
    Bins(usize, usize),
    Narrow,
}

pub struct Analyzer {
    cfg: Config,
    fft: Option<Fft>,
    window: Vec<f32>,
    workspace: Vec<f32>,
    /// What `bands` was made for: (bars, rate, fft size).
    layout: (usize, u32, usize),
    bands: Vec<(Band, f32)>,
    /// Bar indices and Goertzel coefficients for the fractional-bin measurements.
    narrow: Vec<(usize, f64)>,
    targets: Vec<f32>,
    /// Each bar's level before the clamp to 0..1, and the auto level's slide of the scale
    /// (a share of its span: 0.1 is 6 dB of a 60 dB scale).
    raw: Vec<f32>,
    slide: f32,
    levels: Vec<f32>,
    peaks: Vec<f32>,
    /// Per bar: seconds the peak still holds, and its fall speed.
    hold: Vec<f32>,
    speed: Vec<f32>,
}

impl Analyzer {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            fft: None,
            window: Vec::new(),
            workspace: Vec::new(),
            layout: (0, 0, 0),
            bands: Vec::new(),
            narrow: Vec::new(),
            targets: Vec::new(),
            raw: Vec::new(),
            slide: 0.0,
            levels: Vec::new(),
            peaks: Vec::new(),
            hold: Vec::new(),
            speed: Vec::new(),
        }
    }

    /// Change how it reads and moves (the visualizer's look): the levels carry on.
    pub fn set_config(&mut self, cfg: Config) {
        if self.cfg != cfg {
            let bands = (self.cfg.min_hz, self.cfg.max_hz, self.cfg.tilt_db) != (cfg.min_hz, cfg.max_hz, cfg.tilt_db);
            self.cfg = cfg;
            if bands {
                // Lay the bands out again at the next frame.
                self.layout = (0, 0, 0);
            }
        }
    }

    /// The FFT size for a sample rate: about 170 ms of audio, a power of two.
    pub fn fft_size(rate: u32) -> usize {
        ((rate as usize * 17) / 100).next_power_of_two().clamp(2048, 32_768)
    }

    /// The centre frequency of each bar, for `bars` bars.
    pub fn centres(&self, bars: usize) -> Vec<f32> {
        let (lo, hi) = (self.cfg.min_hz, self.cfg.max_hz);
        (0..bars).map(|i| lo * (hi / lo).powf((i as f32 + 0.5) / bars as f32)).collect()
    }

    fn prepare(&mut self, bars: usize, rate: u32) {
        let n = Self::fft_size(rate);
        if self.fft.as_ref().is_none_or(|f| f.n != n) {
            self.fft = Some(Fft::new(n));
            // Hann.
            self.window = (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos()).collect();
            self.workspace = vec![0.0; n];
        }
        if self.layout != (bars, rate, n) {
            self.layout = (bars, rate, n);
            let res = rate as f32 / n as f32;
            let lo = self.cfg.min_hz;
            let hi = self.cfg.max_hz.min(rate as f32 * 0.475);
            self.narrow.clear();
            self.bands = (0..bars)
                .map(|i| {
                    let f0 = lo * (hi / lo).powf(i as f32 / bars as f32);
                    let f1 = lo * (hi / lo).powf((i + 1) as f32 / bars as f32);
                    let centre = (f0 * f1).sqrt();
                    let (b0, b1) = ((f0 / res).ceil() as usize, (f1 / res).ceil() as usize);
                    let band = if b1 > b0 {
                        Band::Bins(b0, b1.min(n / 2))
                    } else {
                        let c = 2.0 * (std::f64::consts::TAU * f64::from(centre / res) / n as f64).cos();
                        self.narrow.push((i, c));
                        Band::Narrow
                    };
                    let tilt = self.cfg.tilt_db * (centre / 1000.0).log2();
                    (band, tilt)
                })
                .collect();
            let resize = |v: &mut Vec<f32>| v.resize(bars, 0.0);
            resize(&mut self.targets);
            resize(&mut self.raw);
            resize(&mut self.levels);
            resize(&mut self.peaks);
            resize(&mut self.hold);
            resize(&mut self.speed);
        }
    }

    /// Transform this window in place, saving the narrow bars' power before the FFT.
    fn transform(&mut self, src: &[f32], norm: f32) {
        let n = self.workspace.len();
        let pad = n - src.len();
        let (zeros, values) = self.workspace.split_at_mut(pad);
        zeros.fill(0.0);
        for ((out, sample), gain) in values.iter_mut().zip(src).zip(&self.window[pad..]) {
            *out = sample * gain;
        }
        // Bars narrower than a bin (the lowest ones): the spectrum at their centre, exactly
        // (Goertzel, in f64: the low frequencies need the precision). Interpolating between
        // bins would lose up to 3 dB of a tone between them. Before the FFT, which works
        // in place.
        for group in self.narrow.chunks(8) {
            // Independent filters share an input, so the compiler can run them in SIMD lanes.
            let mut coeff = [0.0; 8];
            for (slot, &(_, c)) in coeff.iter_mut().zip(group) {
                *slot = c;
            }
            let (mut s1, mut s2) = ([0.0; 8], [0.0; 8]);
            for &v in &self.workspace {
                for k in 0..8 {
                    // The input and older state can be combined while the previous stage
                    // finishes. Use an FMA only where the target supplies the instruction.
                    let addend = f64::from(v) - s2[k];
                    let s0 = if cfg!(any(target_arch = "aarch64", target_feature = "fma")) {
                        coeff[k].mul_add(s1[k], addend)
                    } else {
                        coeff[k] * s1[k] + addend
                    };
                    s2[k] = s1[k];
                    s1[k] = s0;
                }
            }
            for (k, &(bar, c)) in group.iter().enumerate() {
                self.targets[bar] = ((s1[k] * s1[k] + s2[k] * s2[k] - c * s1[k] * s2[k]) as f32) * norm;
            }
        }
        self.fft.as_ref().unwrap().run(&mut self.workspace);
    }

    /// The instant level (0..1) of each of `bars` bars for these samples (mono, oldest
    /// first, at least `fft_size(rate)` of them: the newest are used).
    pub fn measure(&mut self, samples: &[f32], rate: u32, bars: usize) -> &[f32] {
        self.prepare(bars, rate);
        let n = self.workspace.len();
        let src = &samples[samples.len().saturating_sub(n)..];
        // |X|² of a full-scale sine under a Hann window is (N/4)²: that is 0 dB.
        let norm = 16.0 / (n as f32 * n as f32);
        let silent = src.iter().all(|&s| s == 0.0);
        if !silent {
            self.transform(src, norm);
        }
        let power = |k: usize| {
            let [r, i] = self.workspace.as_chunks::<2>().0[k];
            (r * r + i * i) * norm
        };
        let (lo_db, span) = (self.cfg.min_db, self.cfg.max_db - self.cfg.min_db);
        for (b, (band, tilt)) in self.bands.iter().enumerate() {
            let p = match *band {
                _ if silent => 0.0,
                Band::Bins(a, z) => (a..z.max(a + 1).min(n / 2)).map(power).fold(0.0f32, f32::max),
                Band::Narrow => self.targets[b],
            };
            let db = 10.0 * p.max(1e-12).log10() + tilt;
            self.raw[b] = (db - lo_db) / span;
            self.targets[b] = self.raw[b].clamp(0.0, 1.0);
        }
        &self.targets
    }

    /// Move the bars `dt` seconds on: toward these samples' levels, or toward rest
    /// (`None`: paused, stopped).
    pub fn update(&mut self, input: Option<(&[f32], u32)>, bars: usize, dt: f32) {
        match input {
            Some((s, rate)) => {
                self.measure(s, rate, bars);
                if self.cfg.auto_level {
                    self.follow_level(dt);
                }
            }
            None => {
                if self.levels.len() != bars {
                    self.prepare(bars, self.layout.1.max(8_000));
                }
                self.targets.iter_mut().for_each(|t| *t = 0.0);
            }
        }
        let c = &self.cfg;
        let up = 1.0 - (-dt / c.attack.max(1e-4)).exp();
        let down = 1.0 - (-dt / c.decay.max(1e-4)).exp();
        for i in 0..self.levels.len() {
            let (t, l) = (self.targets[i], self.levels[i]);
            let l = l + (t - l) * if t > l { up } else { down };
            self.levels[i] = if l < 1e-4 { 0.0 } else { l };
            if self.levels[i] >= self.peaks[i] {
                self.peaks[i] = self.levels[i];
                self.hold[i] = c.peak_hold;
                self.speed[i] = 0.0;
            } else if self.hold[i] > 0.0 {
                self.hold[i] -= dt;
            } else {
                self.speed[i] += c.peak_fall * dt;
                self.peaks[i] = (self.peaks[i] - self.speed[i] * dt).max(self.levels[i]);
            }
        }
    }

    /// The auto level: the loudest bands (the 85th percentile, so one loud band does not
    /// decide) are brought toward 0.75, the scale sliding up within a quarter of a second
    /// and down over two and a half; never lifting what is nearly silent.
    fn follow_level(&mut self, dt: f32) {
        if self.raw.is_empty() {
            return;
        }
        self.targets.copy_from_slice(&self.raw);
        let index = ((self.raw.len() - 1) as f32 * 0.85) as usize;
        let reference = *self.targets.select_nth_unstable_by(index, f32::total_cmp).1;
        // Nearly silent (below the scale's foot): hold the slide where it is.
        if reference > 0.05 {
            let want = (reference - 0.75).clamp(-0.25, 0.2);
            let tau = if want > self.slide { 0.25 } else { 2.5 };
            self.slide += (want - self.slide) * (1.0 - (-dt / tau).exp());
        }
        for (t, r) in self.targets.iter_mut().zip(&self.raw) {
            *t = (r - self.slide).clamp(0.0, 1.0);
        }
    }

    /// The auto level's slide of the scale, in dB (positive: the music is loud).
    pub fn level_slide_db(&self) -> f32 {
        self.slide * (self.cfg.max_db - self.cfg.min_db)
    }

    pub fn levels(&self) -> &[f32] {
        &self.levels
    }

    pub fn peaks(&self) -> &[f32] {
        &self.peaks
    }

    /// Every bar and cap has fallen (nothing more to draw).
    pub fn at_rest(&self) -> bool {
        self.levels.iter().chain(&self.peaks).all(|&v| v < 0.004)
    }

    /// Drop everything (the visualizer stopped): the next start rises from nothing.
    pub fn reset(&mut self) {
        self.slide = 0.0;
        for v in [&mut self.levels, &mut self.peaks, &mut self.hold, &mut self.speed, &mut self.targets, &mut self.raw] {
            v.iter_mut().for_each(|x| *x = 0.0);
        }
    }
}

/// The frames played at `rate` in `nanos` nanoseconds. In u128: at 192 kHz the product
/// overflows u64 after about 26 hours.
fn frames_in(nanos: u64, rate: u32) -> u64 {
    u64::try_from(u128::from(nanos) * u128::from(rate) / 1_000_000_000).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    #[test]
    fn frames_in_does_not_overflow_after_a_long_idle() {
        use super::frames_in;
        assert_eq!(frames_in(1_000_000_000, 48_000), 48_000);
        let day_and_a_half = 36 * 3600 * 1_000_000_000u64;
        assert_eq!(frames_in(day_and_a_half, 192_000), 36 * 3600 * 192_000);
        assert_eq!(frames_in(u64::MAX, u32::MAX), u64::MAX);
    }

    use super::*;

    fn sine(freq: f32, amp: f32, rate: u32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin()).collect()
    }

    fn loudest(v: &[f32]) -> usize {
        v.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0
    }

    fn flat() -> Config {
        Config { tilt_db: 0.0, ..Config::default() }
    }

    #[test]
    fn fft_matches_a_direct_dft() {
        let n = 64;
        let f = Fft::new(n);
        let x: Vec<f32> = (0..n).map(|i| ((i * 7 % 13) as f32 - 6.0) / 6.0).collect();
        let mut spectrum = x.clone();
        f.run(&mut spectrum);
        for (k, [re, im]) in spectrum.as_chunks::<2>().0.iter().enumerate() {
            let (mut r, mut i) = (0.0f64, 0.0f64);
            for (t, v) in x.iter().enumerate() {
                let a = -2.0 * std::f64::consts::PI * (k * t) as f64 / n as f64;
                r += f64::from(*v) * a.cos();
                i += f64::from(*v) * a.sin();
            }
            assert!((f64::from(*re) - r).abs() < 1e-3 && (f64::from(*im) - i).abs() < 1e-3, "bin {k}");
        }
    }

    #[test]
    fn narrow_bars_match_the_direct_dft_at_their_centres() {
        let (rate, bars) = (48_000, 216);
        let samples = pinkish(0.5, Analyzer::fft_size(rate));
        let mut a = Analyzer::new(flat());
        a.measure(&samples, rate, bars);
        let centres = a.centres(bars);
        assert!(a.narrow.len() > 8); // Exercise more than one batch.
        for &(bar, _) in &a.narrow {
            let step = std::f64::consts::TAU * f64::from(centres[bar]) / f64::from(rate);
            let (mut re, mut im) = (0.0, 0.0);
            for (i, (&s, &w)) in samples.iter().zip(&a.window).enumerate() {
                let (sin, cos) = (step * i as f64).sin_cos();
                re += f64::from(s * w) * cos;
                im += f64::from(s * w) * sin;
            }
            let db = 20.0 * (re.hypot(im) * 4.0 / samples.len() as f64).log10() as f32;
            let want = ((db - a.cfg.min_db) / (a.cfg.max_db - a.cfg.min_db)).clamp(0.0, 1.0);
            assert!((a.targets[bar] - want).abs() < 1e-4, "bar {bar}: {} vs {want}", a.targets[bar]);
        }
    }

    #[test]
    fn a_sine_lands_in_its_bar_at_its_level() {
        for rate in [44_100, 48_000, 96_000, 192_000] {
            let mut a = Analyzer::new(flat());
            let bars = 64;
            let centres = a.centres(bars);
            for (freq, amp_db) in [(60.0f32, -16.0f32), (1000.0, -20.0), (8000.0, -30.0)] {
                let amp = 10f32.powf(amp_db / 20.0);
                let n = Analyzer::fft_size(rate);
                let levels = a.measure(&sine(freq, amp, rate, n), rate, bars).to_vec();
                let i = loudest(&levels);
                let (lo, hi) = (centres[i] / (centres[1] / centres[0]).sqrt(), centres[i] * (centres[1] / centres[0]).sqrt());
                // In its bar; a tone right on the edge between two (within 2%) may light either.
                assert!((lo / 1.02..=hi * 1.02).contains(&freq), "{rate} Hz: {freq} Hz in bar {i} ({lo:.1}-{hi:.1})");
                // The level: (dB - min) / span, within the window's scalloping (1.5 dB).
                let c = Config::default();
                let want = (amp_db - c.min_db) / (c.max_db - c.min_db);
                assert!((levels[i] - want).abs() < 1.5 / (c.max_db - c.min_db), "{rate} Hz, {freq} Hz: {} vs {want}", levels[i]);
                // Bars far from it stay low (the window's leakage is far below the scale).
                let far: Vec<usize> = (0..bars).filter(|&j| (j as i32 - i as i32).abs() > 6).collect();
                assert!(far.iter().all(|&j| levels[j] < 0.15), "{rate} Hz, {freq} Hz leaks: {levels:?}");
            }
        }
    }

    #[test]
    fn a_full_scale_sine_reads_zero_db_and_the_tilt_lifts_the_treble() {
        let mut a = Analyzer::new(Config { min_db: -60.0, max_db: 0.0, tilt_db: 0.0, ..Config::default() });
        let l = a.measure(&sine(1000.0, 1.0, 48_000, 8192), 48_000, 48).to_vec();
        assert!((l[loudest(&l)] - 1.0).abs() < 0.03, "{}", l[loudest(&l)]);
        // With the tilt, -20 dB at 8 kHz (three octaves up) reads 9 dB louder; at 1 kHz the same.
        let mut t = Analyzer::new(Config { min_db: -60.0, max_db: 0.0, tilt_db: 3.0, ..Config::default() });
        let mut f = Analyzer::new(Config { min_db: -60.0, max_db: 0.0, tilt_db: 0.0, ..Config::default() });
        let s = sine(8000.0, 0.1, 48_000, 8192);
        let (lt, lf) = (t.measure(&s, 48_000, 48).to_vec(), f.measure(&s, 48_000, 48).to_vec());
        let i = loudest(&lf);
        let tilt_at = 3.0 * (a.centres(48)[i] / 1000.0).log2() / 60.0;
        assert!((lt[i] - lf[i] - tilt_at).abs() < 0.01, "{} {} {tilt_at}", lt[i], lf[i]);
    }

    #[test]
    fn silence_is_flat() {
        let mut a = Analyzer::new(Config::default());
        // Silence immediately after music must discard both FFT and narrow-band power.
        let signal = pinkish(0.8, 8192);
        assert!(a.measure(&signal, 48_000, 80).iter().any(|&v| v > 0.0));
        let l = a.measure(&vec![0.0; 8192], 48_000, 80);
        assert!(l.iter().all(|&v| v == 0.0));
        // Very quiet noise (-90 dBFS) is below the scale too.
        let mut seed = 1u32;
        let noise: Vec<f32> = (0..8192)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed as f32 / u32::MAX as f32 - 0.5) * 6e-5
            })
            .collect();
        assert!(a.measure(&noise, 48_000, 80).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn bars_rise_fast_fall_smoothly_and_come_to_rest() {
        let mut a = Analyzer::new(flat());
        let s = sine(1000.0, 0.3, 48_000, 8192);
        let dt = 1.0 / 60.0;
        a.update(Some((&s, 48_000)), 40, dt);
        let i = loudest(a.levels());
        let target = a.targets[i];
        // Three frames (50 ms) reach nearly all of it.
        a.update(Some((&s, 48_000)), 40, dt);
        a.update(Some((&s, 48_000)), 40, dt);
        assert!(a.levels()[i] > 0.85 * target, "{} of {target}", a.levels()[i]);
        for _ in 0..30 {
            a.update(Some((&s, 48_000)), 40, dt);
        }
        let top = a.levels()[i];
        // Paused: it falls, never jumps, every frame lower, the cap above it a while.
        let mut last = top;
        let mut frames = 0;
        let mut half_at = None;
        while !a.at_rest() {
            a.update(None, 40, dt);
            frames += 1;
            let l = a.levels()[i];
            assert!(l <= last && last - l < 0.12, "frame {frames}: {last} -> {l}");
            assert!(a.peaks()[i] >= l);
            if half_at.is_none() && l < top / 2.0 {
                half_at = Some(frames);
            }
            last = l;
            assert!(frames < 200, "comes to rest");
        }
        let half = half_at.unwrap();
        assert!((6..=14).contains(&half), "half height after {half} frames");
        assert!(frames <= 120, "at rest within 2 s: {frames} frames");
        // The cap held for its hold time before falling.
        let mut b = Analyzer::new(flat());
        for _ in 0..30 {
            b.update(Some((&s, 48_000)), 40, dt);
        }
        let p = b.peaks()[i];
        for _ in 0..20 {
            b.update(None, 40, dt);
        }
        assert_eq!(b.peaks()[i], p, "held for {} s", Config::default().peak_hold);
    }

    /// Pink-ish noise: falling 3 dB an octave, as music roughly does.
    fn pinkish(amp: f32, n: usize) -> Vec<f32> {
        let mut seed = 7u32;
        let (mut b0, mut b1, mut b2) = (0.0f32, 0.0f32, 0.0f32);
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let w = seed as f32 / u32::MAX as f32 - 0.5;
                b0 = 0.99765 * b0 + w * 0.099_046;
                b1 = 0.963 * b1 + w * 0.296_516_4;
                b2 = 0.57 * b2 + w * 1.052_691_3;
                (b0 + b1 + b2 + w * 0.1848) * amp
            })
            .collect()
    }

    #[test]
    fn the_auto_level_keeps_loud_music_off_the_top_and_quiet_music_moving() {
        let cfg = Config { auto_level: true, ..Config::default() };
        let dt = 1.0 / 60.0;
        let top = |a: &Analyzer| {
            let mut v = a.levels().to_vec();
            v.sort_by(f32::total_cmp);
            v[(v.len() - 1) * 85 / 100]
        };
        // Very loud: with a fixed scale most bars are pinned; following the level they sit
        // under the top.
        let loud = pinkish(4.0, 8192);
        let mut fixed = Analyzer::new(Config::default());
        let mut auto = Analyzer::new(cfg.clone());
        for _ in 0..300 {
            fixed.update(Some((&loud, 48_000)), 64, dt);
            auto.update(Some((&loud, 48_000)), 64, dt);
        }
        let pinned = |a: &Analyzer| a.levels().iter().filter(|&&l| l > 0.97).count();
        assert!(pinned(&fixed) > 20, "fixed: {} pinned", pinned(&fixed));
        // Louder than the slide's 12 dB can take in full: under the top all the same.
        assert!(pinned(&auto) <= 2 && top(&auto) < 0.88, "auto: {} pinned, 85th percentile {}", pinned(&auto), top(&auto));
        assert!(auto.level_slide_db() > 5.0 && auto.level_slide_db() <= 12.01);
        // 20 dB quieter: lower at once, then lifted a little (at most 15 dB), still lower
        // than the loud part was.
        let quiet = pinkish(0.4, 8192);
        let before = top(&auto);
        auto.update(Some((&quiet, 48_000)), 64, dt);
        for _ in 0..20 {
            auto.update(Some((&quiet, 48_000)), 64, dt);
        }
        let dropped = top(&auto);
        assert!(dropped < before - 0.15, "{before} -> {dropped}");
        for _ in 0..600 {
            auto.update(Some((&quiet, 48_000)), 64, dt);
        }
        let lifted = top(&auto);
        assert!(lifted > dropped + 0.05 && lifted <= 0.8, "{dropped} -> {lifted}");
        // Silence is not lifted: flat.
        let mut s = Analyzer::new(cfg);
        for _ in 0..300 {
            s.update(Some((&vec![0.0; 8192], 48_000)), 64, dt);
        }
        assert!(s.levels().iter().all(|&l| l == 0.0));
    }

    #[test]
    fn a_new_look_keeps_the_levels_moving() {
        let mut a = Analyzer::new(Config::default());
        let s = sine(1000.0, 0.3, 48_000, 8192);
        for _ in 0..10 {
            a.update(Some((&s, 48_000)), 40, 1.0 / 60.0);
        }
        let before = a.levels().to_vec();
        a.set_config(Config { decay: 0.5, ..Config::default() });
        assert_eq!(a.levels(), &before[..], "timing changes keep the bars where they are");
        a.update(Some((&s, 48_000)), 40, 1.0 / 60.0);
        let loud = loudest(a.levels());
        assert_eq!(loud, loudest(&before));
    }

    #[test]
    fn the_tap_reads_what_is_heard() {
        let tap = Tap::new();
        // Inactive: nothing is taken.
        tap.push(&[1.0, 1.0], 48_000);
        assert_eq!(tap.written.load(Ordering::Relaxed), 0);
        tap.set_active(true);
        let frames: Vec<f32> = (0..1000).flat_map(|i| [i as f32, i as f32 + 2.0]).collect();
        tap.push(&frames, 44_100);
        // The device still holds the last 100 frames.
        tap.publish(100);
        let mut out = vec![0.0; 4];
        assert_eq!(tap.read(&mut out, false), 44_100);
        assert_eq!(out, [896.0 + 1.0, 897.0 + 1.0, 898.0 + 1.0, 899.0 + 1.0], "mono, ending where the ear is");
        // More than was ever written: zeros first.
        let mut out = vec![9.0; 1002];
        tap.read(&mut out, false);
        assert_eq!(&out[..102], &[0.0; 102][..]);
        assert_eq!(out[102], 1.0);
        // Running: the heard point moves on, never past what was written.
        std::thread::sleep(std::time::Duration::from_millis(50));
        let mut out = vec![0.0; 1];
        tap.read(&mut out, true);
        assert_eq!(out[0], 999.0 + 1.0);
    }
}
