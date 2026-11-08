//! A force layout for the map, after d3-force: bodies repel (Barnes–Hut), links pull, overlaps push apart,
//! sessions drift to their project's anchor and quiet ones to an outer ring. Call `tick` once per frame while
//! `running`.

pub struct Body {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    /// Held in place (a restored position while newcomers settle).
    pub pinned: bool,
    /// Collision radius.
    pub r: f32,
    /// Negative repels.
    pub charge: f32,
    pub anchor: (f32, f32),
    pub anchor_pull: f32,
    /// Drifts to the outer ring instead of an anchor.
    pub ring: bool,
}

pub struct Link {
    pub s: usize,
    pub t: usize,
    pub distance: f32,
    pub strength: f32,
}

pub struct Sim {
    pub bodies: Vec<Body>,
    pub links: Vec<Link>,
    pub alpha: f32,
    pub ring_r: f32,
}

const ALPHA_MIN: f32 = 0.001;
const ALPHA_DECAY: f32 = 0.025;
const VELOCITY_KEEP: f32 = 0.6;
const THETA2: f32 = 0.81;
const DISTANCE_MAX2: f32 = 480.0 * 480.0;
const RING_PULL: f32 = 0.12;

/// A quadtree cell: its bounds, children, the bodies of a leaf, and its total charge and center.
struct Quad {
    x0: f32,
    y0: f32,
    size: f32,
    kids: [u32; 4],
    bodies: Vec<u32>,
    charge: f32,
    cx: f32,
    cy: f32,
}

const NONE: u32 = u32::MAX;

impl Sim {
    pub fn running(&self) -> bool {
        self.alpha >= ALPHA_MIN
    }

    pub fn tick(&mut self) {
        if self.bodies.is_empty() {
            self.alpha = 0.0;
            return;
        }
        self.alpha += (0.0 - self.alpha) * ALPHA_DECAY;
        let alpha = self.alpha;
        self.charge(alpha);
        self.pull(alpha);
        self.collide();
        let ring = self.ring_r;
        for b in &mut self.bodies {
            if b.ring {
                let r = (b.x * b.x + b.y * b.y).sqrt().max(1e-3);
                let k = (ring - r) * RING_PULL * alpha / r;
                (b.vx, b.vy) = (b.vx + b.x * k, b.vy + b.y * k);
            } else if b.anchor_pull > 0.0 {
                b.vx += (b.anchor.0 - b.x) * b.anchor_pull * alpha;
                b.vy += (b.anchor.1 - b.y) * b.anchor_pull * alpha;
            }
            if b.pinned {
                (b.vx, b.vy) = (0.0, 0.0);
            } else {
                b.vx *= VELOCITY_KEEP;
                b.vy *= VELOCITY_KEEP;
                b.x += b.vx;
                b.y += b.vy;
            }
        }
    }

    fn tree(&self) -> Vec<Quad> {
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for b in &self.bodies {
            (x0, y0, x1, y1) = (x0.min(b.x), y0.min(b.y), x1.max(b.x), y1.max(b.y));
        }
        let size = (x1 - x0).max(y1 - y0).max(1.0) + 1.0;
        let mut q = vec![Quad { x0, y0, size, kids: [NONE; 4], bodies: Vec::new(), charge: 0.0, cx: 0.0, cy: 0.0 }];
        for (i, b) in self.bodies.iter().enumerate() {
            let mut at = 0;
            for depth in 0.. {
                let leaf = q[at].kids == [NONE; 4];
                if leaf && (q[at].bodies.is_empty() || depth >= 24) {
                    q[at].bodies.push(i as u32);
                    break;
                }
                if leaf {
                    // Split: move the leaf's body one level down.
                    let moved = std::mem::take(&mut q[at].bodies);
                    for j in moved {
                        let k = Self::child(&mut q, at, self.bodies[j as usize].x, self.bodies[j as usize].y);
                        q[k].bodies.push(j);
                    }
                }
                at = Self::child(&mut q, at, b.x, b.y);
            }
        }
        // Totals, children first (they come after their parent in the vector).
        for at in (0..q.len()).rev() {
            let (mut charge, mut wx, mut wy, mut w) = (0.0, 0.0, 0.0, 0.0);
            for &j in &q[at].bodies {
                let b = &self.bodies[j as usize];
                (charge, wx, wy, w) = (charge + b.charge, wx + b.x * b.charge.abs(), wy + b.y * b.charge.abs(), w + b.charge.abs());
            }
            for k in q[at].kids {
                if k != NONE {
                    let c = &q[k as usize];
                    (charge, wx, wy, w) = (charge + c.charge, wx + c.cx * c.charge.abs(), wy + c.cy * c.charge.abs(), w + c.charge.abs());
                }
            }
            q[at].charge = charge;
            if w > 0.0 {
                (q[at].cx, q[at].cy) = (wx / w, wy / w);
            }
        }
        q
    }

    fn child(q: &mut Vec<Quad>, at: usize, x: f32, y: f32) -> usize {
        let half = q[at].size / 2.0;
        let (right, down) = (x >= q[at].x0 + half, y >= q[at].y0 + half);
        let i = usize::from(right) + 2 * usize::from(down);
        if q[at].kids[i] == NONE {
            let (x0, y0) = (q[at].x0 + if right { half } else { 0.0 }, q[at].y0 + if down { half } else { 0.0 });
            q.push(Quad { x0, y0, size: half, kids: [NONE; 4], bodies: Vec::new(), charge: 0.0, cx: 0.0, cy: 0.0 });
            q[at].kids[i] = (q.len() - 1) as u32;
        }
        q[at].kids[i] as usize
    }

    /// Many-body repulsion: far groups of bodies act as one.
    fn charge(&mut self, alpha: f32) {
        let q = self.tree();
        let mut stack = Vec::new();
        for i in 0..self.bodies.len() {
            let (x, y) = (self.bodies[i].x, self.bodies[i].y);
            let (mut vx, mut vy) = (0.0, 0.0);
            stack.clear();
            stack.push(0usize);
            while let Some(at) = stack.pop() {
                let c = &q[at];
                if c.charge == 0.0 {
                    continue;
                }
                let (dx, dy) = (c.cx - x, c.cy - y);
                let l = dx * dx + dy * dy;
                let internal = c.kids != [NONE; 4];
                if internal && c.size * c.size / THETA2 < l {
                    if l < DISTANCE_MAX2 {
                        let l = l.max(1.0);
                        (vx, vy) = (vx + dx * c.charge * alpha / l, vy + dy * c.charge * alpha / l);
                    }
                    continue;
                }
                if internal {
                    stack.extend(c.kids.iter().filter(|k| **k != NONE).map(|k| *k as usize));
                    continue;
                }
                for &j in &c.bodies {
                    let j = j as usize;
                    if j == i {
                        continue;
                    }
                    let o = &self.bodies[j];
                    let (mut dx, dy) = (o.x - x, o.y - y);
                    if dx == 0.0 && dy == 0.0 {
                        dx = ((i * 31 + j * 17) % 7) as f32 * 1e-3 - 3e-3; // two bodies on one spot: nudge apart
                    }
                    let l = dx * dx + dy * dy;
                    if l < DISTANCE_MAX2 {
                        let l = l.max(1.0);
                        (vx, vy) = (vx + dx * o.charge * alpha / l, vy + dy * o.charge * alpha / l);
                    }
                }
            }
            self.bodies[i].vx += vx;
            self.bodies[i].vy += vy;
        }
    }

    /// Links pull their ends to their distance; the end with fewer links moves more.
    fn pull(&mut self, alpha: f32) {
        let mut count = vec![0u32; self.bodies.len()];
        for l in &self.links {
            count[l.s] += 1;
            count[l.t] += 1;
        }
        for l in &self.links {
            let (s, t) = (&self.bodies[l.s], &self.bodies[l.t]);
            let (mut x, mut y) = (t.x + t.vx - s.x - s.vx, t.y + t.vy - s.y - s.vy);
            let d = (x * x + y * y).sqrt().max(1e-3);
            let k = (d - l.distance) / d * alpha * l.strength;
            (x, y) = (x * k, y * k);
            let bias = count[l.s] as f32 / (count[l.s] + count[l.t]).max(1) as f32;
            let t = &mut self.bodies[l.t];
            (t.vx, t.vy) = (t.vx - x * bias, t.vy - y * bias);
            let s = &mut self.bodies[l.s];
            (s.vx, s.vy) = (s.vx + x * (1.0 - bias), s.vy + y * (1.0 - bias));
        }
    }

    /// Overlapping bodies push apart, the smaller one more. A sweep along x finds the pairs.
    fn collide(&mut self) {
        let n = self.bodies.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|a, b| (self.bodies[*a].x + self.bodies[*a].vx).total_cmp(&(self.bodies[*b].x + self.bodies[*b].vx)));
        let max_r = self.bodies.iter().map(|b| b.r).fold(0.0, f32::max);
        for a in 0..n {
            let i = order[a];
            for &j in &order[a + 1..] {
                let (bi, bj) = (&self.bodies[i], &self.bodies[j]);
                let (xi, yi, xj, yj) = (bi.x + bi.vx, bi.y + bi.vy, bj.x + bj.vx, bj.y + bj.vy);
                if xj - xi > bi.r + max_r {
                    break;
                }
                let r = bi.r + bj.r;
                let (dx, dy) = (xi - xj, yi - yj);
                let l2 = dx * dx + dy * dy;
                if l2 >= r * r || l2 == 0.0 {
                    continue;
                }
                let l = l2.sqrt();
                let k = (r - l) / l;
                let (ri2, rj2) = (bi.r * bi.r, bj.r * bj.r);
                let share = rj2 / (ri2 + rj2);
                let (px, py) = (dx * k, dy * k);
                let bi = &mut self.bodies[i];
                (bi.vx, bi.vy) = (bi.vx + px * share, bi.vy + py * share);
                let bj = &mut self.bodies[j];
                (bj.vx, bj.vy) = (bj.vx - px * (1.0 - share), bj.vy - py * (1.0 - share));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(x: f32, y: f32) -> Body {
        Body { x, y, vx: 0.0, vy: 0.0, pinned: false, r: 10.0, charge: -200.0, anchor: (0.0, 0.0), anchor_pull: 0.0, ring: false }
    }

    #[test]
    fn settles_apart_and_linked() {
        let mut sim =
            Sim { bodies: (0..40).map(|i| body((i % 7) as f32, (i / 7) as f32)).collect(), links: vec![], alpha: 1.0, ring_r: 300.0 };
        sim.links.push(Link { s: 0, t: 1, distance: 85.0, strength: 0.45 });
        let mut ticks = 0;
        while sim.running() && ticks < 1000 {
            sim.tick();
            ticks += 1;
        }
        assert!(!sim.running() && ticks < 400, "{ticks} ticks");
        for (i, a) in sim.bodies.iter().enumerate() {
            for b in &sim.bodies[i + 1..] {
                assert!((a.x - b.x).hypot(a.y - b.y) > 15.0, "two bodies still overlap");
            }
        }
        let (a, b) = (&sim.bodies[0], &sim.bodies[1]);
        assert!((a.x - b.x).hypot(a.y - b.y) < 200.0, "the link should hold its ends together");
    }
}
