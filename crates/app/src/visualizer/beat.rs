//! Hearing the beat: where the kicks are, and the tempo they keep.
//!
//! The spectrum the visualizer draws is a 170 ms window, smoothed again as it moves: a kick
//! in it is a slow swell, told late and vaguely. This listens to the sound itself, the
//! newest 35 ms of it each frame:
//!
//! - **Kicks.** The energy at five frequencies from 45 to 110 Hz (Goertzel, under a Hann
//!   window: no state between frames, so it does not matter that successive frames overlap
//!   or by how much). A kick is that energy rising sharply (`Riser`): the rise summed over
//!   the frames it lasts, told the moment it passes what a kick needs (so a frame or two
//!   after the kick, not after its peak), no weaker than a share of what recent kicks'
//!   rises were, and reaching near the loudest the band has lately been (a bass note between
//!   two kicks is not one, nor is the band swelling back after two low notes cancelled for
//!   a moment). Its strength is how far past what it needed the rise went.
//! - **Snaps.** The same for the sound's sharp end (the energy of its sample-to-sample
//!   change): a snare, a clap, a rim shot; a hi-hat under a snare is too quiet to be one.
//!   Shown as sparks, not as beats; and they help find the tempo.
//! - **Tempo.** The kicks and snaps of the last seconds, each a point on a circle turned
//!   once per candidate beat: for the right period they pile up on one side
//!   (`|Σ w e^(2πi t/P)|`), and where they pile up is the beat's phase. Kicks between the
//!   beats (a hip-hop bar's second kick, half a beat late) are counted on the circle turned
//!   twice and four times per beat, for less. Half the period fits as well, so a period is
//!   also judged by how many of its beats have something on them, and by how usual it is
//!   (84 to 152 a minute: of a tempo and its double, the one in that range), and by
//!   whether what was heard comes round again four beats later, as a bar does. Kept steady: a small change is eased in, a large one must be
//!   heard twice. Let go four seconds after the last kick.
//! - **Beats without a kick.** With the tempo known, a beat whose kick is missing (a bar's
//!   one left out, a kick under a loud bass) is still told, softer, as long as a kick was
//!   heard lately: a breakdown is not filled with beats nobody played.
//!
//! No allocation after the first frames, a few thousand multiplications a frame.

use std::collections::VecDeque;

/// The kick band's frequencies (Hz).
const KICK_HZ: [f32; 5] = [45.0, 58.0, 72.0, 90.0, 110.0];
/// The window listened to (seconds).
const WINDOW: f32 = 0.035;
/// What a rise must reach to be a kick (natural log of the energy's growth: 1.0 is 2.7
/// times the energy, 4.3 dB), and the share of recent kicks' rises it must reach as well.
const RISE: f32 = 1.0;
const SHARE: f32 = 0.42;
/// The least time between two kicks (seconds).
const APART: f32 = 0.11;
/// For this long after one, another must be as loud as it was (seconds).
const AGAIN: f32 = 0.3;
/// How far under the band's recent loudest a kick may be (natural log of the energy: 1.6 is
/// 7 dB), and how fast that loudest is forgotten (per second: a quieter passage's kicks are
/// kicks again within a second or two).
const UNDER: f32 = 1.6;
const FORGET: f32 = 0.4;
/// The most a rise counts for (the first sound after silence is a huge one, and no measure
/// of the kicks that follow).
const MOST: f32 = 4.5;
/// Below these energies nothing is a kick (about -62 dBFS) or a snap.
const FLOOR: f32 = 6e-7;
const SNAP_FLOOR: f32 = 2e-7;
/// How long a kick or snap counts toward the tempo (seconds), and how many are kept.
const MEMORY: f64 = 8.0;
const KEPT: usize = 64;
/// The tempos considered (beats a minute).
const BPM: (f32, f32) = (68.0, 184.0);

/// What was heard this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Heard {
    /// A beat to show: a kick's strength (0.3 to 1), or a beat told from the tempo alone
    /// (softer); 0 for none.
    pub hit: f32,
    /// It was a kick (not a beat told from the tempo).
    pub kick: bool,
    /// A snare, a clap: its strength (0.3 to 1), 0 for none.
    pub snap: f32,
    /// The beat's length in seconds, 0 while no tempo is known.
    pub period: f32,
    /// Where in the beat this frame is (0 at the beat, toward 1 before the next).
    pub phase: f32,
}

impl Heard {
    /// Beats a minute, 0 while no tempo is known.
    #[cfg_attr(not(any(test, feature = "bench")), allow(dead_code))]
    pub fn bpm(&self) -> f32 {
        if self.period > 0.0 { 60.0 / self.period } else { 0.0 }
    }
}

/// A band's energy over time, and the sharp rises in it.
struct Riser {
    /// Last frame's energy (its log), and the rise under way.
    last: f32,
    rise: f32,
    /// The rise under way has been told.
    told: bool,
    /// What recent rises came to (a slow average).
    usual: f32,
    /// The loudest the band has lately been (its log), forgotten slowly.
    top: f32,
    /// Since the last one told, and how loud that one got (its log, followed for the 80 ms
    /// after it was told: it is told before its peak).
    since: f32,
    peak: f32,
    started: bool,
}

impl Default for Riser {
    fn default() -> Self {
        Riser { last: 0.0, rise: 0.0, told: false, usual: 0.0, top: f32::MIN, since: 9.0, peak: f32::MIN, started: false }
    }
}

impl Riser {
    /// This frame's energy: the strength of a rise told now (0.3 to 1), or 0.
    fn step(&mut self, energy: f32, floor: f32, dt: f32) -> f32 {
        self.since += dt;
        // Never under the floor: a rise out of silence is counted from there.
        let level = energy.max(floor * 0.5).ln();
        let grew = if self.started { level - self.last } else { 0.0 };
        (self.started, self.last) = (true, level);
        self.top = level.max(self.top - dt * FORGET);
        if self.since < 0.08 {
            self.peak = self.peak.max(level);
        }
        // Right after one, only another as loud: a kick's tail swelling back as it beats
        // against a bass note is not a second kick, a second kick is.
        let again = self.since >= AGAIN || level >= self.peak - 0.35;
        let mut told = 0.0;
        if grew > 0.02 {
            // The rise: summed while the energy keeps growing.
            self.rise = (self.rise + grew).min(MOST);
            let needs = RISE.max(self.usual * SHARE);
            if !self.told && self.rise >= needs && level > floor.ln() && level >= self.top - UNDER && self.since >= APART && again {
                self.told = true;
                self.since = 0.0;
                self.peak = level;
                told = (0.4 + (self.rise - needs) * 0.3).clamp(0.3, 1.0);
            }
        } else {
            if self.told {
                // What this one's rise came to, for judging the next ones.
                self.usual += (self.rise - self.usual) * if self.usual == 0.0 { 1.0 } else { 0.3 };
            }
            (self.rise, self.told) = (0.0, false);
        }
        // Too rare to judge by: the usual rise eases back, so a quieter passage's are heard.
        self.usual *= (-dt / 6.0).exp();
        told
    }

    /// Nothing playing: no rise carries over.
    fn rest(&mut self, dt: f32) {
        self.since += dt;
        (self.rise, self.told, self.started) = (0.0, false, false);
    }
}

pub struct Tracker {
    /// Seconds listened to.
    now: f64,
    /// The Hann window for the length in use.
    window: Vec<f32>,
    low: Riser,
    high: Riser,
    /// The kicks and snaps kept for the tempo: when, how much each counts, and whether it
    /// was a kick.
    heard: VecDeque<(f64, f32, bool)>,
    /// The tempo: the beat's length (0: none), a beat's time, and how well it fits.
    period: f32,
    origin: f64,
    fit: f32,
    /// A different tempo heard once, waiting to be heard again.
    other: f32,
    /// Until the tempo is worked out again.
    wait: f32,
    /// The last beat counted (its number from `origin`), and when a beat was last shown.
    counted: i64,
    shown: f64,
}

impl Default for Tracker {
    fn default() -> Self {
        Tracker { now: 0.0, window: Vec::new(), low: Riser::default(), high: Riser::default(), heard: VecDeque::with_capacity(KEPT + 1), period: 0.0, origin: 0.0, fit: 0.0, other: 0.0, wait: 0.0, counted: 0, shown: -9.0 }
    }
}

impl Tracker {
    /// Forget everything (the visualizer stopped, another track).
    pub fn reset(&mut self) {
        let window = std::mem::take(&mut self.window);
        *self = Tracker { window, ..Tracker::default() };
    }

    /// A frame with nothing playing: time passes, nothing is heard.
    pub fn rest(&mut self, dt: f32) -> Heard {
        let dt = dt.max(0.0);
        self.now += f64::from(dt);
        self.low.rest(dt);
        self.high.rest(dt);
        if self.low.since > 3.0 {
            self.period = 0.0;
        }
        Heard::default()
    }

    /// A frame: `samples` are the newest being heard (mono, oldest first), `dt` seconds
    /// after the last frame's.
    pub fn update(&mut self, samples: &[f32], rate: u32, dt: f32) -> Heard {
        let dt = dt.clamp(0.0, 0.25);
        self.now += f64::from(dt);
        let (low, high) = self.energies(samples, rate);
        let kick = self.low.step(low, FLOOR, dt);
        let snap = self.high.step(high, SNAP_FLOOR, dt);
        for (strength, is_kick) in [(kick, true), (snap * 0.6, false)] {
            if strength > 0.0 {
                self.heard.push_back((self.now, strength, is_kick));
                if self.heard.len() > KEPT {
                    self.heard.pop_front();
                }
            }
        }
        while self.heard.front().is_some_and(|k| self.now - k.0 > MEMORY) {
            self.heard.pop_front();
        }
        self.wait -= dt;
        if self.wait <= 0.0 {
            self.wait = 0.3;
            self.tempo();
        }
        // The beat's clock: a beat counted when its time comes.
        let (mut phase, mut tick) = (0.0, false);
        if self.period > 0.0 {
            let at = (self.now - self.origin) / f64::from(self.period);
            let n = at.floor() as i64;
            phase = (at - at.floor()) as f32;
            tick = n != self.counted;
            self.counted = n;
        }
        // What to show: a kick, unless a beat was just shown for it (the tempo told it a
        // moment early); or a beat without a kick, when a kick was heard lately.
        let near = f64::from(self.period) * 0.3;
        let mut hit = 0.0;
        if kick > 0.0 {
            if self.period == 0.0 || self.now - self.shown > near {
                hit = kick;
            }
        } else if tick && self.fit > 0.6 && self.now - self.shown > f64::from(self.period) * 0.7 && self.low.since < self.period * 1.6 && self.low.rise < RISE * 0.5 {
            hit = 0.45;
        }
        if hit > 0.0 {
            self.shown = self.now;
        }
        Heard { hit, kick: kick > 0.0, snap, period: self.period, phase }
    }

    /// In the newest `WINDOW` of `samples`: the energy at the kick band's frequencies (a
    /// full-scale sine at one of them: 1), and the energy of the sample-to-sample change
    /// (the sound's sharp end: a snare's, a clap's).
    fn energies(&mut self, samples: &[f32], rate: u32) -> (f32, f32) {
        let n = ((rate as f32 * WINDOW) as usize).min(samples.len());
        if n < 64 {
            return (0.0, 0.0);
        }
        if self.window.len() != n {
            self.window = (0..n).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos()).collect();
        }
        let src = &samples[samples.len() - n..];
        let norm = 16.0 / (n as f32 * n as f32);
        // Window each sample once and advance the independent kick filters together.
        let coeff = KICK_HZ.map(|hz| 2.0 * (std::f32::consts::TAU * hz / rate as f32).cos());
        let (mut s1, mut s2) = ([0.0; KICK_HZ.len()], [0.0; KICK_HZ.len()]);
        for (&x, &w) in src.iter().zip(&self.window) {
            let sample = x * w;
            for k in 0..KICK_HZ.len() {
                let s0 = sample + coeff[k] * s1[k] - s2[k];
                s2[k] = s1[k];
                s1[k] = s0;
            }
        }
        let low = (0..KICK_HZ.len())
            .map(|k| (s1[k] * s1[k] + s2[k] * s2[k] - coeff[k] * s1[k] * s2[k]) * norm)
            .sum::<f32>().max(0.0);
        // The newer half only: a snap is short, and half the window tells it sooner.
        let recent = &src[n / 2..];
        let high = recent.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum::<f32>() / recent.len() as f32;
        (low, high)
    }

    /// The tempo that what was heard keeps, if it keeps one.
    fn tempo(&mut self) {
        // No kick for four seconds: whatever tempo was held has stopped being played (a
        // breakdown, the track's end); it is found again from the next kicks.
        let last_kick = self.heard.iter().rev().find(|k| k.2).map(|k| k.0);
        if last_kick.is_none_or(|t| self.now - t > 4.0) {
            (self.period, self.fit, self.other) = (0.0, 0.0, 0.0);
            return;
        }
        if self.heard.len() < 4 || self.heard.iter().filter(|k| k.2).count() < 2 {
            return;
        }
        // Each onset's weight is the same for every candidate tempo.
        let mut weights = [0.0; KEPT];
        for (w, &(time, strength, _)) in weights.iter_mut().zip(&self.heard) {
            *w = f64::from(strength) * (-(self.now - time) / 5.0).exp();
        }
        let weighted = || self.heard.iter().zip(weights.iter().copied());
        let total = weighted().map(|(_, w)| w).sum::<f64>().max(1e-9);
        let kicks = weighted().filter(|(k, _)| k.2).map(|(_, w)| w).sum::<f64>().max(1e-9);
        let (first, last) = (self.heard.front().unwrap().0, self.heard.back().unwrap().0);
        let mut on = [0.0f32; 64];
        let mut best = (0.0f32, 0.0f32, 0.0f32, 0.0f64);
        let mut bpm = BPM.0;
        while bpm <= BPM.1 {
            let p = 60.0 / f64::from(bpm);
            // How well what was heard lies on the beats, on the half beats, on the quarters;
            // and the kicks alone on the beats.
            let mut z = [(0.0f64, 0.0f64); 3];
            let mut zk = (0.0f64, 0.0f64);
            for (k, w) in weighted() {
                let angle = std::f64::consts::TAU * (k.0 / p);
                let (mut sin, mut cos) = angle.sin_cos();
                if k.2 {
                    zk.0 += w * cos;
                    zk.1 += w * sin;
                }
                for harmonic in &mut z {
                    harmonic.0 += w * cos;
                    harmonic.1 += w * sin;
                    // The next circle turns twice as fast.
                    (sin, cos) = (2.0 * sin * cos, cos * cos - sin * sin);
                }
            }
            let size = |v: (f64, f64), of: f64| ((v.0 * v.0 + v.1 * v.1).sqrt() / of) as f32;
            // The beats are where the kicks are (hats between them do not make the tempo
            // twice as fast), or, when the kicks are off the beat as often as on it, where
            // everything together is.
            let (by_kicks, by_all) = (size(zk, kicks), size(z[0], total));
            let on_beat = if by_kicks >= by_all { zk } else { z[0] };
            let align = 0.55 * by_kicks.max(by_all) + 0.3 * size(z[1], total) + 0.15 * size(z[2], total);
            // A beat's time: where they pile up on the circle turned once per beat.
            let origin = on_beat.1.atan2(on_beat.0) / std::f64::consts::TAU * p;
            // How many of the period's beats have something on them: a kick, or (for less)
            // a snap.
            let beats = (((last - first) / p).round() as usize + 1).clamp(1, on.len());
            on[..beats].fill(0.0);
            let base = ((first - origin) / p).round();
            for k in &self.heard {
                let at = (k.0 - origin) / p;
                let i = at.round() - base;
                if (at - at.round()).abs() < 0.17 && i >= 0.0 && (i as usize) < beats {
                    on[i as usize] = on[i as usize].max(if k.2 { 1.0 } else { 0.6 });
                }
            }
            let kept = on[..beats].iter().sum::<f32>() / beats as f32;
            // Whether it comes round again a bar later (four beats): what was heard, and the
            // same kind of thing four beats before or after it. A tempo that only happens to
            // put things on a grid does not repeat.
            let bar = 4.0 * p;
            let (mut again, mut could) = (0.0f64, 0.0f64);
            for (k, w) in weighted() {
                let (before, after) = (k.0 - bar, k.0 + bar);
                if before < first - 0.08 && after > last + 0.08 {
                    continue;
                }
                could += w;
                if self.heard.iter().any(|j| j.2 == k.2 && ((j.0 - before).abs() < 0.07 || (j.0 - after).abs() < 0.07)) {
                    again += w;
                }
            }
            let repeats = if could > 0.0 { (again / could) as f32 } else { 0.5 };
            let fit = align * kept.powf(0.7) * (0.35 + 0.65 * repeats);
            // As usual as any from 84 to 152 a minute; less so the farther outside (twice or
            // half a tempo fits the same kicks: the one in that range is the one meant).
            let outside = if bpm < 84.0 { (84.0 / bpm).log2() } else if bpm > 152.0 { (bpm / 152.0).log2() } else { 0.0 };
            let usual = (-0.5 * (outside / 0.22).powi(2)).exp();
            if fit * usual > best.0 {
                best = (fit * usual, fit, p as f32, origin);
            }
            bpm += 0.5;
        }
        let (_, fit, period, origin) = best;
        if fit < 0.36 {
            // No tempo in this: the one held is let go once it has stopped fitting.
            self.fit *= 0.7;
            if self.fit < 0.3 {
                self.period = 0.0;
            }
            return;
        }
        if self.period > 0.0 && ((period - self.period) / self.period).abs() < 0.04 {
            // The same tempo, heard a little differently: eased in.
            self.period += (period - self.period) * 0.35;
            self.other = 0.0;
        } else if self.period == 0.0 || (self.other > 0.0 && ((period - self.other) / self.other).abs() < 0.04) {
            // A tempo where there was none, or another one heard twice.
            self.period = period;
            self.other = 0.0;
        } else {
            self.other = period;
            return;
        }
        self.fit = fit;
        // A beat's time near now, on the held period's grid through the phase heard.
        let p = f64::from(self.period);
        self.origin = origin + ((self.now - origin) / p).floor() * p;
        // The count carries on from the new origin without telling a beat twice.
        self.counted = ((self.now - self.origin) / p).floor() as i64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// A made-up track: a kick on every beat (a sine falling from 120 to 50 Hz), a held
    /// bass note under it, a hat between the beats, a pad. `skip`: beats without their kick.
    fn track(bpm: f32, secs: f32, kick: f32, skip: &[usize]) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        let beat = 60.0 / bpm;
        let mut seed = 0x1234_5678u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        };
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let (b, ph) = ((t / beat).floor() as usize, t % beat);
                let k = if skip.contains(&b) { 0.0 } else { kick * (-ph / 0.11).exp() * (std::f32::consts::TAU * (50.0 * ph + 70.0 * 0.05 * (1.0 - (-ph / 0.05).exp()))).sin() };
                let bass = 0.12 * (std::f32::consts::TAU * 55.0 * t).sin();
                let off = (t + beat / 2.0) % beat;
                let hat = 0.15 * (-off / 0.02).exp() * noise();
                let pad = 0.08 * (std::f32::consts::TAU * 440.0 * t).sin() + 0.05 * (std::f32::consts::TAU * 660.0 * t).sin();
                k + bass + hat + pad
            })
            .collect()
    }

    /// The tracker run over a track at `fps` frames a second: every frame's `Heard`, timed.
    fn listen(sound: &[f32], fps: f32) -> Vec<(f32, Heard)> {
        let mut tr = Tracker::default();
        let (dt, size) = (1.0 / fps, 8192usize);
        let mut out = Vec::new();
        let mut t = 0.0f32;
        while ((t * RATE as f32) as usize) < sound.len() {
            let end = (t * RATE as f32) as usize;
            let mut buf = vec![0.0; size];
            let from = end.saturating_sub(size);
            buf[size - (end - from)..].copy_from_slice(&sound[from..end]);
            out.push((t, tr.update(&buf, RATE, dt)));
            t += dt;
        }
        out
    }

    #[test]
    fn every_kick_is_heard_once_and_soon() {
        for (bpm, fps) in [(120.0, 60.0), (92.0, 60.0), (150.0, 60.0), (174.0, 60.0), (120.0, 30.0), (128.0, 144.0)] {
            let sound = track(bpm, 10.0, 0.8, &[]);
            let heard = listen(&sound, fps);
            let kicks: Vec<f32> = heard.iter().filter(|h| h.1.kick).map(|h| h.0).collect();
            let beat = 60.0 / bpm;
            let expected = (10.0 / beat).floor() as usize;
            assert!((expected - 1..=expected + 1).contains(&kicks.len()), "{bpm} at {fps}: {} kicks for {expected} beats", kicks.len());
            // Each within 70 ms after its kick began (the window filling, a frame or two).
            for k in &kicks {
                let late = k % beat;
                assert!(late < 0.07 + 1.0 / fps, "{bpm} at {fps}: a kick told {late} s after the beat at {k}");
            }
        }
    }

    #[test]
    fn the_tempo_is_found_and_kept() {
        for bpm in [84.0, 100.0, 120.0, 128.0, 140.0, 174.0] {
            let sound = track(bpm, 12.0, 0.8, &[]);
            let heard = listen(&sound, 60.0);
            let at = |secs: f32| heard.iter().find(|h| h.0 >= secs).unwrap().1;
            assert!((at(6.0).bpm() - bpm).abs() < 2.0, "{bpm}: {} after 6 s", at(6.0).bpm());
            assert!((at(11.5).bpm() - bpm).abs() < 1.5, "{bpm}: {} after 11.5 s", at(11.5).bpm());
            // The phase is 0 at the beats: right after a kick it is small.
            let last = heard.iter().rev().find(|h| h.1.kick).unwrap();
            assert!(last.1.phase < 0.2 || last.1.phase > 0.9, "{bpm}: phase {} at a kick", last.1.phase);
        }
    }

    #[test]
    fn a_missing_kick_is_still_a_beat_and_a_breakdown_is_not() {
        // Beats 12 and 16 without their kick: told from the tempo, softer, once each.
        let sound = track(120.0, 12.0, 0.8, &[12, 16]);
        let heard = listen(&sound, 60.0);
        for b in [12.0f32, 16.0] {
            let near: Vec<&(f32, Heard)> = heard.iter().filter(|h| h.1.hit > 0.0 && (h.0 - b * 0.5).abs() < 0.12).collect();
            assert!(near.len() == 1 && !near[0].1.kick && near[0].1.hit < 0.6, "beat {b}: {near:?}");
        }
        // Every other beat shown is a kick, one per beat.
        let shown = heard.iter().filter(|h| h.1.hit > 0.0).count();
        assert!((22..=25).contains(&shown), "{shown}");
        // The kicks stop at 6 s: one beat more at the most, then nothing.
        let skip: Vec<usize> = (12..40).collect();
        let sound = track(120.0, 12.0, 0.8, &skip);
        let heard = listen(&sound, 60.0);
        let after = heard.iter().filter(|h| h.1.hit > 0.0 && h.0 > 6.2).count();
        assert!(after <= 1, "{after} beats shown in the breakdown");
        assert!(heard.iter().find(|h| h.0 >= 8.0).unwrap().1.period > 0.0, "the tempo is held a while (the kicks may come back in time)");
        assert_eq!(heard.last().unwrap().1.period, 0.0, "and let go four seconds after the last kick");
    }

    /// A hip-hop bar: kicks on the one and half a beat after the three, snares (noise) on
    /// the two and the four, hats on the quarters of each beat, a long low note under it.
    fn hiphop(bpm: f32, secs: f32) -> Vec<f32> {
        let n = (secs * RATE as f32) as usize;
        let beat = 60.0 / bpm;
        let mut seed = 0x2468_ace1u32;
        let mut noise = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0
        };
        let since = |t: f32, at: f32, every: f32| (t - at).rem_euclid(every);
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let bar = beat * 4.0;
                let thump = |ph: f32, amp: f32| amp * (-ph / 0.11).exp() * (std::f32::consts::TAU * (50.0 * ph + 70.0 * 0.05 * (1.0 - (-ph / 0.05).exp()))).sin();
                let kicks = thump(since(t, 0.0, bar), 0.8) + thump(since(t, beat * 2.5, bar), 0.7);
                let sub = 0.3 * (std::f32::consts::TAU * 31.0 * t).sin();
                let nz = noise();
                let snare = 0.5 * ((-since(t, beat, bar) / 0.05).exp() + (-since(t, beat * 3.0, bar) / 0.05).exp()) * nz;
                let hat = 0.06 * (-since(t, 0.0, beat / 4.0) / 0.012).exp() * nz;
                kicks + sub + snare + hat
            })
            .collect()
    }

    #[test]
    fn kicks_off_the_beat_and_snares_on_it_still_tell_the_tempo() {
        for bpm in [88.0, 96.0] {
            let heard = listen(&hiphop(bpm, 14.0), 60.0);
            let at = |secs: f32| heard.iter().find(|h| h.0 >= secs).unwrap().1;
            assert!((at(8.0).bpm() - bpm).abs() < 2.5, "{bpm}: {} after 8 s", at(8.0).bpm());
            assert!((at(13.5).bpm() - bpm).abs() < 2.0, "{bpm}: {} after 13.5 s", at(13.5).bpm());
            // The snares are snaps, not kicks: two a bar, on the two and the four.
            // (After the first bars: until a snare has been heard, a hat is the sharpest thing.)
            let bars = 11.0 / (240.0 / bpm);
            let snaps = heard.iter().filter(|h| h.1.snap > 0.0 && h.0 >= 3.0).count() as f32;
            let kicks = heard.iter().filter(|h| h.1.kick && h.0 >= 3.0).count() as f32;
            assert!((snaps / bars - 2.0).abs() < 0.5 && (kicks / bars - 2.0).abs() < 0.5, "{bpm}: {snaps} snaps and {kicks} kicks in {bars} bars");
        }
    }

    #[test]
    fn nothing_is_heard_in_what_has_no_kicks() {
        // A held bass note, a pad and hiss; silence; a kick far too quiet to be one.
        let sound = track(120.0, 8.0, 0.0, &[]);
        let heard = listen(&sound, 60.0);
        assert!(heard.iter().filter(|h| h.1.hit > 0.0).count() <= 1, "{:?}", heard.iter().filter(|h| h.1.hit > 0.0).collect::<Vec<_>>());
        assert_eq!(heard.last().unwrap().1.period, 0.0);
        let heard = listen(&vec![0.0; RATE as usize * 3], 60.0);
        assert!(heard.iter().all(|h| h.1 == Heard::default()));
        // Paused: nothing, and what was held is let go.
        let mut tr = Tracker::default();
        let sound = track(120.0, 6.0, 0.8, &[]);
        for (i, _) in listen(&sound, 60.0).iter().enumerate() {
            let end = ((i as f32 / 60.0) * RATE as f32) as usize;
            tr.update(&sound[end.saturating_sub(8192)..end.max(1)], RATE, 1.0 / 60.0);
        }
        assert!(tr.period > 0.0);
        for _ in 0..240 {
            assert_eq!(tr.rest(1.0 / 60.0), Heard::default());
        }
        assert_eq!(tr.period, 0.0);
    }

    #[test]
    fn a_kick_is_stronger_the_harder_it_hits_and_a_bass_note_between_kicks_is_not_one() {
        let strength = |kick: f32| {
            let heard = listen(&track(120.0, 6.0, kick, &[]), 60.0);
            let k: Vec<f32> = heard.iter().filter(|h| h.1.kick).map(|h| h.1.hit.max(0.3)).collect();
            k.iter().sum::<f32>() / k.len().max(1) as f32
        };
        assert!(strength(0.9) >= strength(0.25), "{} {}", strength(0.9), strength(0.25));
        // Small bumps of bass between the kicks (a tenth of a kick's energy): not kicks.
        let mut sound = track(120.0, 8.0, 0.8, &[]);
        for (i, s) in sound.iter_mut().enumerate() {
            let t = i as f32 / RATE as f32;
            let ph = (t + 0.25) % 0.5;
            *s += 0.1 * (-ph / 0.08).exp() * (std::f32::consts::TAU * 80.0 * t).sin();
        }
        let kicks = listen(&sound, 60.0).iter().filter(|h| h.1.kick && h.0 > 2.0).count();
        assert!((11..=13).contains(&kicks), "{kicks} kicks in 6 s at 120");
    }
}
