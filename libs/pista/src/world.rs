// SPDX-FileCopyrightText: 2026 Edwin F. Veliz Jaramillo (lynxcraft) and AriCodeOS contributors
// SPDX-License-Identifier: Apache-2.0
//! The race: karts, their physics at a fixed 60 steps a second, laps and
//! positions. The same input gives the same race, on any machine.

use crate::fixed::{cos, f, isqrt, length, mul, ratio, sin, F, ONE};
use crate::track::{Track, HALF_WIDTH, SEGMENTS};

pub const KARTS: usize = 4;
pub const LAPS: u32 = 3;
/// Steps per second.
pub const RATE: i32 = 60;
/// Steps of "3, 2, 1".
pub const COUNTDOWN: u32 = 3 * RATE as u32;

const TOP_SPEED: F = f(34);
const OFF_ROAD_SPEED: F = f(13);
const BOOST_SPEED: F = f(46);
const ACCEL: F = f(12);
const BRAKE: F = f(32);
const DRAG: F = f(4);
const OFF_ROAD_DRAG: F = f(30);
/// Heading change per step at full lock, in 1/65536 of a turn.
const TURN: i32 = 330;
/// How much of the slip the course takes up each step, in 1/16: most of it
/// with grip, little in a slide.
const GRIP: i32 = 10;
const DRIFT_GRIP: i32 = 2;
const DRIFT_TURN_NUM: i64 = 3;
const DRIFT_TURN_DEN: i64 = 2;
/// Steps of speed a turbo strip gives.
const BOOST_STEPS: u32 = 80;
const TOO_FAR: F = HALF_WIDTH + f(2);
const KART_RADIUS: F = f(1);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Input {
    pub throttle: bool,
    pub brake: bool,
    pub left: bool,
    pub right: bool,
    pub drift: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kart {
    pub x: F,
    pub y: F,
    pub z: F,
    /// Where it points, and where it is going: they differ in a slide.
    pub heading: u16,
    pub course: u16,
    pub speed: F,
    /// The wheels' lock, from -ONE to ONE.
    pub steer: F,
    pub segment: usize,
    pub laps: u32,
    /// Ordered gates prevent reverse crossings and shortcuts from scoring laps.
    pub checkpoint: u8,
    pub started: bool,
    pub drift_charge: u32,
    pub drifting: bool,
    pub recovery: u32,
    /// Recovery ignores a held steering key until it is released.
    pub steer_armed: bool,
    offroad_steps: u32,
    safe_segment: usize,
    pub lap_start: u32,
    pub last_lap: u32,
    pub best_lap: u32,
    pub boost: u32,
    /// The step it finished at.
    pub finished: Option<u32>,
    /// How near its top speed a computer drives, and which side of the road.
    skill: F,
    lane: F,
}

impl Kart {
    /// Laps and segments, for positions.
    pub fn progress(&self) -> i64 {
        (self.laps as i64 - i64::from(!self.started)) * SEGMENTS as i64 + self.segment as i64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Countdown,
    Racing,
    Finished,
}

pub struct World {
    pub track: Track,
    pub karts: [Kart; KARTS],
    pub phase: Phase,
    pub step: u32,
    /// Steps since the green light.
    pub clock: u32,
    launch_charge: u32,
}

/// Whether segment `i` has a turbo strip.
pub fn is_turbo(i: usize) -> bool {
    matches!(i % SEGMENTS, 60..=63 | 250..=253 | 395..=398)
}

impl World {
    pub fn new() -> Self {
        let track = Track::new();
        let start = SEGMENTS - 6;
        let kart = |row: usize, side: i32, skill: F, driving_lane: F| {
            let i = start - row * 2;
            let s = *track.at(i);
            let n = track.at(i + 1);
            let lane = f(side) * 3;
            let heading = heading_of(n.x - s.x, n.z - s.z);
            Kart {
                x: s.x + mul(lane, s.right_x),
                y: s.y,
                z: s.z + mul(lane, s.right_z),
                heading,
                course: heading,
                speed: 0,
                steer: 0,
                segment: i,
                laps: 0,
                checkpoint: 0,
                started: false,
                drift_charge: 0,
                drifting: false,
                recovery: 0,
                steer_armed: true,
                offroad_steps: 0,
                safe_segment: i,
                lap_start: 0,
                last_lap: 0,
                best_lap: 0,
                boost: 0,
                finished: None,
                skill,
                lane: driving_lane,
            }
        };
        let karts = [
            kart(3, 1, ONE, 0),
            kart(0, -1, ratio(90, 100), -f(4)),
            kart(1, 1, ratio(93, 100), f(4)),
            kart(2, -1, ratio(96, 100), f(1)),
        ];
        Self {
            track,
            karts,
            phase: Phase::Countdown,
            step: 0,
            clock: 0,
            launch_charge: 0,
        }
    }

    /// What a computer does with kart `i`: aims ahead along the road, and
    /// lifts for corners.
    pub fn autopilot(&self, i: usize) -> Input {
        let k = &self.karts[i];
        let ahead = 3 + (k.speed >> 19) as usize;
        let s = self.track.at(k.segment + ahead);
        let lane = k.lane;
        let (tx, tz) = (
            s.x + mul(lane, s.right_x) - k.x,
            s.z + mul(lane, s.right_z) - k.z,
        );
        let dist = length(tx >> 2, tz >> 2).max(1);
        // The sine of the angle to the target: right is positive.
        let side = ((mul(tx >> 2, cos(k.heading)) as i64 - mul(tz >> 2, sin(k.heading)) as i64)
            * ONE as i64
            / dist as i64) as F;
        let band = ONE / 24;
        // How far the road ahead turns away from here: the cosine between
        // the two directions, so a corner is seen before it is reached.
        let (here, far) = (self.track.at(k.segment), self.track.at(k.segment + 16));
        let bend = mul(here.right_x, far.right_x) + mul(here.right_z, far.right_z);
        let target = if bend < ratio(60, 100) {
            f(17)
        } else if bend < ratio(85, 100) {
            f(24)
        } else {
            mul(TOP_SPEED, k.skill)
        };
        Input {
            throttle: k.speed < target,
            brake: k.speed > target + f(2),
            left: side < -band,
            right: side > band,
            drift: false,
        }
    }

    /// One step, with the player's kart (kart 0) on `player`.
    pub fn advance(&mut self, player: Input) {
        if self.phase == Phase::Finished {
            return;
        }
        self.step += 1;
        if self.phase == Phase::Countdown {
            self.launch_charge = if player.throttle {
                self.launch_charge + 1
            } else {
                0
            };
            if self.step >= COUNTDOWN {
                self.phase = Phase::Racing;
                // Timing the throttle in the final half-second earns a launch.
                if (8..=30).contains(&self.launch_charge) {
                    self.karts[0].boost = 50;
                    self.karts[0].speed = f(12);
                }
            }
            return;
        }
        self.clock += 1;
        for i in 0..KARTS {
            let input = if i == 0 && self.karts[0].finished.is_none() {
                player
            } else {
                self.autopilot(i)
            };
            self.drive(i, input);
        }
        self.collide();
        let clock = self.clock;
        for k in &mut self.karts {
            if k.finished.is_none() && k.laps >= LAPS {
                k.finished = Some(clock);
            }
        }
        if self.karts[0].finished.is_some() {
            self.phase = Phase::Finished;
        }
    }

    fn drive(&mut self, i: usize, input: Input) {
        let mut k = self.karts[i];
        let lateral = self.track.lateral(k.segment, k.x, k.z);
        let off_road = lateral.abs() > HALF_WIDTH;

        // Speed.
        let dt = |v: F| v / RATE;
        let top = if off_road {
            OFF_ROAD_SPEED
        } else if k.boost > 0 {
            BOOST_SPEED
        } else {
            TOP_SPEED
        };
        if input.throttle && k.speed < top {
            k.speed += dt(ACCEL);
        } else if !input.throttle {
            k.speed -= dt(DRAG);
        }
        if input.brake {
            k.speed -= dt(BRAKE);
        }
        if k.speed > top {
            k.speed -= dt(if off_road { OFF_ROAD_DRAG } else { DRAG * 3 });
        }
        k.speed = k.speed.max(0);
        k.boost = k.boost.saturating_sub(1);

        // Steering: the wheels take time to turn, and a stopped kart cannot.
        if !input.left && !input.right {
            k.steer_armed = true;
        }
        let wanted = match (input.left && k.steer_armed, input.right && k.steer_armed) {
            (true, false) => -ONE,
            (false, true) => ONE,
            _ => 0,
        };
        k.steer += (wanted - k.steer).clamp(-ONE / 8, ONE / 8);
        let drifting = input.drift && k.steer_armed && k.speed > f(10) && !off_road;
        if drifting && k.steer.abs() > ONE / 3 {
            k.drift_charge = (k.drift_charge + 1).min(120);
        } else if !input.drift && k.drifting && !off_road {
            let reward = match k.drift_charge {
                90.. => 90,
                40.. => 45,
                _ => 0,
            };
            if reward > 0 {
                k.boost = k.boost.max(reward);
                k.speed = (k.speed + f(7)).min(BOOST_SPEED);
            }
            k.drift_charge = 0;
        } else if !drifting {
            k.drift_charge = 0;
        }
        k.drifting = drifting;
        k.recovery = k.recovery.saturating_sub(1);
        let mut turn = (k.steer as i64 * TURN as i64) >> 16;
        turn = turn * (k.speed.min(f(14)) as i64) / f(14) as i64;
        if drifting {
            turn = turn * DRIFT_TURN_NUM / DRIFT_TURN_DEN;
        }
        k.heading = k.heading.wrapping_add(turn as i16 as u16);

        // Its course follows its heading: at once with grip, late in a slide.
        let grip = if drifting { DRIFT_GRIP } else { GRIP };
        let slip = k.heading.wrapping_sub(k.course) as i16 as i32;
        k.course = k.course.wrapping_add((slip * grip / 16) as i16 as u16);

        // Position.
        let step = dt(k.speed);
        k.x += mul(step, sin(k.course));
        k.z += mul(step, cos(k.course));

        // Where that is on the circuit.
        let before = k.segment;
        let candidate = self.track.nearest(before, k.x, k.z);
        // Only road travel advances gates or the last safe recovery point.
        let on_road = self.track.lateral(candidate, k.x, k.z).abs() <= HALF_WIDTH;
        k.segment = candidate;
        if on_road {
            Self::cross_gates(&mut k, before, self.clock);
            k.safe_segment = candidate;
            k.offroad_steps = 0;
        } else {
            k.offroad_steps += 1;
            k.boost = 0;
        }
        let s = *self.track.at(k.segment);
        k.y += (s.y - k.y) / 4;
        if is_turbo(k.segment) && on_road {
            k.boost = BOOST_STEPS;
        }

        // The far grass ends: back towards the road.
        let lateral = self.track.lateral(k.segment, k.x, k.z);
        if lateral.abs() > TOO_FAR {
            let back = lateral - lateral.clamp(-TOO_FAR, TOO_FAR);
            k.x -= mul(back, s.right_x);
            k.z -= mul(back, s.right_z);
            // Slide against the visible barrier, instead of crossing through it.
            k.speed = k.speed.min(f(8));
            k.boost = 0;
        }
        if k.offroad_steps >= 60 {
            k.segment = k.safe_segment;
            self.reset_kart(&mut k);
        }
        self.karts[i] = k;
    }

    fn cross_gates(k: &mut Kart, before: usize, clock: u32) {
        let travel = (k.segment + SEGMENTS - before) % SEGMENTS;
        if travel == 0 || travel > 24 {
            return; // Reverse travel cannot open a gate.
        }
        let crossed = |gate: usize| {
            let distance = (gate + SEGMENTS - before) % SEGMENTS;
            distance > 0 && distance <= travel
        };
        if crossed(0) {
            if k.started && k.checkpoint == 3 {
                k.laps += 1;
                k.last_lap = clock - k.lap_start;
                if k.best_lap == 0 || k.last_lap < k.best_lap {
                    k.best_lap = k.last_lap;
                }
            }
            k.started = true;
            k.checkpoint = 0;
            k.lap_start = clock;
        }
        if k.started && k.checkpoint < 3 && crossed((k.checkpoint as usize + 1) * SEGMENTS / 4) {
            k.checkpoint += 1;
        }
    }

    /// Returns to the current section without advancing any checkpoint.
    /// Losing speed and any charged turbo is the cost of recovery.
    pub fn recover(&mut self) {
        if self.phase != Phase::Racing {
            return;
        }
        let mut k = self.karts[0];
        k.segment = k.safe_segment;
        self.reset_kart(&mut k);
        self.karts[0] = k;
    }

    fn reset_kart(&self, k: &mut Kart) {
        let s = self.track.at(k.segment);
        let n = self.track.at(k.segment + 1);
        k.x = s.x;
        k.y = s.y;
        k.z = s.z;
        k.heading = heading_of(n.x - s.x, n.z - s.z);
        k.course = k.heading;
        k.speed = 0;
        k.steer = 0;
        k.boost = 0;
        k.drift_charge = 0;
        k.drifting = false;
        k.recovery = 60;
        k.offroad_steps = 0;
        k.steer_armed = false;
    }

    pub fn wrong_way(&self) -> bool {
        let k = &self.karts[0];
        let s = self.track.at(k.segment);
        k.speed > f(3) && mul(cos(k.heading), s.right_x) - mul(sin(k.heading), s.right_z) < -ONE / 3
    }

    /// Karts that touch push apart, and lose a little speed.
    fn collide(&mut self) {
        for a in 0..KARTS {
            for b in a + 1..KARTS {
                let (dx, dz) = (
                    self.karts[b].x - self.karts[a].x,
                    self.karts[b].z - self.karts[a].z,
                );
                if dx.abs() > 2 * KART_RADIUS || dz.abs() > 2 * KART_RADIUS {
                    continue;
                }
                let d = isqrt((dx as i64 * dx as i64 + dz as i64 * dz as i64) as u64) as F;
                let overlap = 2 * KART_RADIUS - d;
                if overlap <= 0 {
                    continue;
                }
                // Identical centres still need a direction in which to separate.
                let (dx, dz, d) = if d == 0 { (ONE, 0, ONE) } else { (dx, dz, d) };
                let push = |v: F| ((v as i64 * (overlap / 2) as i64) / d as i64) as F;
                let (px, pz) = (push(dx), push(dz));
                self.karts[a].x -= px;
                self.karts[a].z -= pz;
                self.karts[b].x += px;
                self.karts[b].z += pz;
                self.karts[a].speed -= self.karts[a].speed / 64;
                self.karts[b].speed -= self.karts[b].speed / 64;
            }
        }
    }

    /// Kart 0's place: 1 is first.
    pub fn position(&self) -> usize {
        let you = &self.karts[0];
        let ahead = |k: &&Kart| match (k.finished, you.finished) {
            (Some(a), Some(b)) => a < b,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => k.progress() > you.progress(),
        };
        1 + self.karts[1..].iter().filter(ahead).count()
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

/// The heading of a direction: a walk around the circle is enough for a start.
pub(crate) fn heading_of(dx: F, dz: F) -> u16 {
    let mut best = (i64::MIN, 0u16);
    for a in (0..=u16::MAX).step_by(64) {
        let d = mul(dx, sin(a)) as i64 + mul(dz, cos(a)) as i64;
        if d > best.0 {
            best = (d, a);
        }
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn race(steps: u32, player: impl Fn(&World) -> Input) -> World {
        let mut w = World::new();
        for _ in 0..steps {
            let input = player(&w);
            w.advance(input);
        }
        w
    }

    #[test]
    fn holding_a_turn_offroad_recovers_without_repeating_the_escape() {
        for left in [false, true] {
            let mut w = race(COUNTDOWN, |_| Input::default());
            let mut rescued = false;
            let k = &mut w.karts[0];
            let s = w.track.at(k.segment);
            let side = if left { -1 } else { 1 };
            k.x = s.x + mul(side * (HALF_WIDTH + ONE), s.right_x);
            k.z = s.z + mul(side * (HALF_WIDTH + ONE), s.right_z);
            k.heading = k.heading.wrapping_add((side * 16384) as i16 as u16);
            k.course = k.heading;
            k.speed = f(20);
            let input = Input {
                throttle: true,
                left,
                right: !left,
                ..Input::default()
            };
            for _ in 0..1800 {
                w.advance(input);
                let k = &w.karts[0];
                assert!(w.track.lateral(k.segment, k.x, k.z).abs() <= TOO_FAR + ONE / 2);
                assert_eq!(k.laps, 0, "circling on grass cannot score a lap");
                if !k.steer_armed {
                    rescued = true;
                    assert_eq!(k.steer, 0);
                    break;
                }
            }
            assert!(rescued, "the stuck kart must return to a safe section");
            w.advance(input);
            assert_eq!(
                w.karts[0].steer, 0,
                "held steering is not replayed on recovery"
            );
            w.advance(Input::default());
            w.advance(input);
            assert_ne!(
                w.karts[0].steer, 0,
                "releasing and pressing rearms steering"
            );
        }
    }

    #[test]
    fn checkpoints_must_be_crossed_in_order_and_forward() {
        let mut k = World::new().karts[0];
        k.segment = 2;
        World::cross_gates(&mut k, SEGMENTS - 2, 10);
        assert!(k.started);
        assert_eq!(k.laps, 0);
        // Skipping the first gate cannot arm the second or score a lap.
        k.segment = SEGMENTS / 2 + 1;
        World::cross_gates(&mut k, SEGMENTS / 2 - 1, 20);
        assert_eq!(k.checkpoint, 0);
        // A reverse crossing cannot arm the first gate either.
        k.segment = SEGMENTS / 4 - 1;
        World::cross_gates(&mut k, SEGMENTS / 4 + 1, 30);
        assert_eq!(k.checkpoint, 0);
        for gate in 1..=3 {
            k.segment = gate * SEGMENTS / 4 + 1;
            World::cross_gates(&mut k, gate * SEGMENTS / 4 - 1, 40 + gate as u32);
            assert_eq!(k.checkpoint, gate as u8);
        }
        k.segment = 1;
        World::cross_gates(&mut k, SEGMENTS - 1, 100);
        assert_eq!(k.laps, 1);
        assert_eq!(k.last_lap, 90);
        assert_eq!(k.best_lap, 90);
        World::cross_gates(&mut k, SEGMENTS - 1, 110);
        assert_eq!(k.laps, 1, "crossing the line twice does not score twice");
    }

    #[test]
    fn release_rewards_a_charged_drift_but_grass_cancels_it() {
        for (charge, reward) in [(39, 0), (40, 45), (90, 90)] {
            let mut w = race(COUNTDOWN, |_| Input::default());
            w.recover();
            w.advance(Input::default());
            w.karts[0].speed = f(20);
            w.karts[0].drifting = true;
            w.karts[0].drift_charge = charge;
            w.advance(Input {
                throttle: true,
                ..Input::default()
            });
            assert_eq!(w.karts[0].boost, reward);
            assert_eq!(w.karts[0].drift_charge, 0);
        }
        let mut w = race(COUNTDOWN, |_| Input::default());
        let k = &mut w.karts[0];
        let s = w.track.at(k.segment);
        k.x = s.x + mul(f(20), s.right_x);
        k.z = s.z + mul(f(20), s.right_z);
        k.speed = f(20);
        k.drifting = true;
        k.drift_charge = 100;
        w.advance(Input::default());
        assert_eq!(w.karts[0].boost, 0);
        assert_eq!(w.karts[0].drift_charge, 0);
    }

    #[test]
    fn recovery_preserves_race_progress_and_costs_speed() {
        let mut w = race(COUNTDOWN + 500, |w| w.autopilot(0));
        let before = w.karts[0];
        w.karts[0].boost = 50;
        w.recover();
        let k = &w.karts[0];
        assert_eq!(
            (k.laps, k.segment, k.checkpoint),
            (before.laps, before.segment, before.checkpoint)
        );
        assert_eq!(w.track.lateral(k.segment, k.x, k.z), 0);
        assert_eq!((k.speed, k.boost, k.drift_charge), (0, 0, 0));
        assert!(!w.wrong_way());
    }

    #[test]
    fn timed_launch_beats_holding_throttle_through_countdown() {
        let held = race(COUNTDOWN, |_| Input {
            throttle: true,
            ..Input::default()
        });
        let timed = race(COUNTDOWN, |w| Input {
            throttle: w.step >= COUNTDOWN - 20,
            ..Input::default()
        });
        assert_eq!(held.karts[0].boost, 0);
        assert!(timed.karts[0].boost > 0);
        assert!(timed.karts[0].speed > held.karts[0].speed);
    }

    #[test]
    fn finished_race_freezes_the_result_and_clock() {
        let mut w = World::new();
        while w.phase != Phase::Finished {
            w.advance(w.autopilot(0));
        }
        let (karts, clock, place) = (w.karts, w.clock, w.position());
        assert!(karts[0].best_lap > 0);
        for _ in 0..120 {
            w.advance(Input::default());
        }
        assert_eq!(w.karts, karts);
        assert_eq!(w.clock, clock);
        assert_eq!(w.position(), place);
    }

    #[test]
    fn identical_kart_centres_separate() {
        let mut w = World::new();
        w.karts[1].x = w.karts[0].x;
        w.karts[1].z = w.karts[0].z;
        w.collide();
        assert!(
            length(w.karts[1].x - w.karts[0].x, w.karts[1].z - w.karts[0].z) >= 2 * KART_RADIUS
        );
    }

    #[test]
    fn nobody_moves_before_the_green_light() {
        let start = World::new().karts;
        let w = race(COUNTDOWN - 1, |_| Input {
            throttle: true,
            ..Input::default()
        });
        assert_eq!(w.karts, start);
        assert_eq!(w.phase, Phase::Countdown);
    }

    #[test]
    fn the_same_input_gives_the_same_race() {
        let drive = |w: &World| w.autopilot(0);
        let (a, b) = (race(2000, drive), race(2000, drive));
        assert_eq!(a.karts, b.karts);
    }

    /// The computer stays on the road and finishes, so the circuit can be
    /// driven, and laps count once each.
    #[test]
    fn a_computer_finishes_all_the_laps_on_the_road() {
        let mut w = World::new();
        let mut off = 0;
        let limit = COUNTDOWN + (240 * RATE) as u32;
        let mut laps_seen = 0;
        while w.phase != Phase::Finished {
            let input = w.autopilot(0);
            w.advance(input);
            let k = &w.karts[0];
            if w.track.lateral(k.segment, k.x, k.z).abs() > HALF_WIDTH {
                off += 1;
            }
            assert!(k.laps >= laps_seen && k.laps <= laps_seen + 1);
            laps_seen = k.laps;
            assert!(w.step < limit, "lap {}, segment {}", k.laps, k.segment);
        }
        assert_eq!(w.karts[0].laps, LAPS);
        assert!(
            off < w.step / 50,
            "off the road for {off} of {} steps",
            w.step
        );
        // About a minute a lap: a race, not a sprint or a march.
        let per_lap = w.clock / LAPS / RATE as u32;
        assert!((15..=70).contains(&per_lap), "{per_lap} s a lap");
    }

    #[test]
    fn the_brake_stops_and_grass_slows() {
        let flat_out = Input {
            throttle: true,
            ..Input::default()
        };
        let w = race(COUNTDOWN + 90, |_| flat_out);
        assert!(w.karts[0].speed > f(15), "{}", w.karts[0].speed >> 16);
        let mut w = w;
        for _ in 0..200 {
            w.advance(Input {
                brake: true,
                ..Input::default()
            });
        }
        assert_eq!(w.karts[0].speed, 0);
        // Driven straight off the road, it falls to the grass speed.
        let mut w = race(COUNTDOWN + 60, |_| flat_out);
        let k = &mut w.karts[0];
        k.x += 30 * w.track.at(k.segment).right_x;
        k.z += 30 * w.track.at(k.segment).right_z;
        k.speed = TOP_SPEED;
        let mut slowest = TOP_SPEED;
        for _ in 0..60 {
            w.advance(flat_out);
            slowest = slowest.min(w.karts[0].speed);
        }
        let k = &w.karts[0];
        assert!(w.track.lateral(k.segment, k.x, k.z).abs() <= TOO_FAR + ONE / 16);
        assert!(
            slowest <= OFF_ROAD_SPEED + f(1),
            "{}",
            w.karts[0].speed >> 16
        );
    }

    #[test]
    fn running_off_never_goes_past_the_edge_of_the_world() {
        let mut w = race(COUNTDOWN, |_| Input::default());
        for _ in 0..3000 {
            w.advance(Input {
                throttle: true,
                right: true,
                drift: true,
                ..Input::default()
            });
            let k = &w.karts[0];
            let lateral = w.track.lateral(k.segment, k.x, k.z);
            assert!(lateral.abs() <= TOO_FAR + f(2), "{}", lateral >> 16);
        }
    }

    #[test]
    fn a_slide_turns_harder_than_grip() {
        let turning = |drift: bool| {
            let mut w = race(COUNTDOWN + 180, |_| Input {
                throttle: true,
                ..Input::default()
            });
            w.recover();
            w.advance(Input::default());
            w.karts[0].speed = f(20);
            let from = w.karts[0].heading;
            for _ in 0..20 {
                w.advance(Input {
                    throttle: true,
                    right: true,
                    drift,
                    ..Input::default()
                });
            }
            w.karts[0].heading.wrapping_sub(from) as i16 as i32
        };
        assert!(turning(true) > turning(false));
    }

    #[test]
    fn karts_that_touch_push_apart() {
        let mut w = World::new();
        w.karts[1].x = w.karts[0].x + ONE / 2;
        w.karts[1].z = w.karts[0].z;
        w.collide();
        let d = length(w.karts[1].x - w.karts[0].x, w.karts[1].z - w.karts[0].z);
        assert!(d > ONE / 2 + ONE / 2, "{d}");
        w.collide();
    }

    #[test]
    fn places_count_who_is_ahead() {
        let mut w = World::new();
        for k in &mut w.karts {
            k.laps = 1;
            k.started = true;
            k.segment = 10;
        }
        w.karts[0].segment = 11;
        assert_eq!(w.position(), 1);
        w.karts[2].laps = 2;
        assert_eq!(w.position(), 2);
        w.karts[1].finished = Some(100);
        w.karts[0].finished = Some(200);
        assert_eq!(w.position(), 2);
    }
}
