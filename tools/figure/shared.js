// The model of the bench and the parts of it that move, used by the build to draw the
// figure at rest and by the page to draw it as it runs. It is read as source in both
// places, so it names the kit's functions bare (cylinder, solidSvg, ...) and exports nothing.

// Film wound on a hub: each wrap adds one thickness, so the area of the pack is what
// measures how much is on it.
const HUB = 12, FULL = 40, PITCH = 3.5;
const WOUND = FULL * FULL - HUB * HUB;
const LENGTH = (Math.PI * WOUND) / PITCH;
const radiusOf = (share) => Math.sqrt(HUB * HUB + WOUND * share);
const wrapsOf = (share) => (radiusOf(share) - HUB) / PITCH;

// A leader stays on each reel, so the film is always threaded.
const LEADER = 0.04;
const shareAt = (through) => LEADER + (1 - 2 * LEADER) * through;

// Two damaged stretches, by where along the film they begin and how long they are (in
// world units of film), and how far each pulls the gauge down.
const DAMAGE = [
  { at: 0.31, long: 18, drop: 31 },
  { at: 0.66, long: 11, drop: 17 },
];
const APERTURE = 2.5;
// Where a point of the film is along the straight run, measured from the gate, when
// `through` of the film has passed.
const placeOf = (at, through) => (through - at) * LENGTH * (1 - 2 * LEADER);
// What the gauge reads: 100 with sound film in the gate, less by how much of the gate's
// opening a damaged stretch covers.
function soundAt(through) {
  let reads = 100;
  for (const d of DAMAGE) {
    const head = placeOf(d.at, through), tail = head - d.long;
    const covered = Math.max(0, Math.min(APERTURE, head) - Math.max(-APERTURE, tail)) / (APERTURE * 2);
    reads -= d.drop * covered;
  }
  return reads;
}

const SUPPLY = [-92, -8], TAKEUP = [92, -8];
const GUIDES = [[-34, 44], [34, 44]];
const GUIDE_R = 4.5;
const LINE_Y = GUIDES[0][1] + GUIDE_R;
const PACK_Z = 6, PACK_H = 7, HUB_TOP = 15;
const FILM_Z = 6.8, FILM_H = 5.6;
const DIAL = [0, 33], DIAL_R = 10.5, DIAL_TOP = 22;
const RULE = [-100, 40];

// The tangent that runs along the outside of two circles, on the side the film is on.
function tangent(c1, r1, c2, r2) {
  const dx = c2[0] - c1[0], dy = c2[1] - c1[1], far = Math.hypot(dx, dy);
  const c = (r1 - r2) / far, s = Math.sqrt(1 - c * c);
  const n = [(dx * c - dy * s) / far, (dy * c + dx * s) / far];
  return { n, from: [c1[0] + n[0] * r1, c1[1] + n[1] * r1], to: [c2[0] + n[0] * r2, c2[1] + n[1] * r2] };
}

// A strip of film standing on its edge along a path on the floor.
function ribbon(points, P) {
  const thick = 0.45, inner = [];
  for (let i = 0; i < points.length; i++) {
    const a = points[Math.max(0, i - 1)], b = points[Math.min(points.length - 1, i + 1)];
    const far = Math.hypot(b[0] - a[0], b[1] - a[1]) || 1;
    inner.push([points[i][0] + ((b[1] - a[1]) / far) * thick, points[i][1] - ((b[0] - a[0]) / far) * thick]);
  }
  return solidSvg(extrude([...points, ...inner.reverse()], FILM_Z, FILM_H, P, { convex: false }), { tone: "hi", lit: true, crease: "none" });
}

const around = (c, r, from, to, steps) => Array.from({ length: steps + 1 }, (_, i) => { const a = from + ((to - from) * i) / steps; return [c[0] + Math.cos(a) * r, c[1] + Math.sin(a) * r]; });

// The film from a reel to its guide roller and round the roller to the straight run.
function runSvg(side, share, P) {
  const reel = side < 0 ? SUPPLY : TAKEUP, guide = side < 0 ? GUIDES[0] : GUIDES[1];
  const t = side < 0 ? tangent(reel, radiusOf(share), guide, GUIDE_R) : tangent(guide, GUIDE_R, reel, radiusOf(share));
  const angle = Math.atan2(t.n[1], t.n[0]);
  const bend = around(guide, GUIDE_R, side < 0 ? angle : Math.PI / 2, side < 0 ? Math.PI / 2 : angle, 5);
  return ribbon(side < 0 ? [t.from, ...bend] : [...bend, t.to], P);
}

// The film wound on a reel, with a ring for every few wraps.
function packSvg(reel, share, P) {
  const r = radiusOf(share);
  let rings = "";
  for (let at = HUB + PITCH; at < r - 1.2; at += PITCH) rings += ring(reel[0], reel[1], at, PACK_Z + PACK_H, P, 44);
  return solidSvg(cylinder(reel[0], reel[1], r, PACK_Z, PACK_H, P, 64, 0.5), { tone: "hi", crease: "lo" }) + lineSvg(rings, { tone: "faint" });
}

// What shows a hub is turning: a key from the nut to the rim, and three holes.
function hubSvg(reel, turn, P) {
  const at = (r, a) => [reel[0] + Math.cos(a) * r, reel[1] + Math.sin(a) * r];
  const key = lineOnTop(at(4.4, turn), at(10, turn), HUB_TOP, P);
  const holes = [1, 3, 5].map((n) => iso([...at(7, turn + (n * Math.PI) / 3), HUB_TOP], P));
  return lineSvg(key, { tone: "hi" }) + dotsSvg(holes, { size: 0.7, tone: "mid" });
}

// Scratches across a damaged stretch while it is on its way to the gate. Past the gate
// the film is sound again.
function damageSvg(through, P) {
  let d = "";
  for (const mark of DAMAGE) {
    const head = placeOf(mark.at, through);
    for (let x = head - mark.long + 1.5; x <= head - 1; x += 3) {
      if (x > GUIDES[0][0] + 1 && x < -APERTURE) d += segment([x, LINE_Y + 0.05, FILM_Z], [x + 1.4, LINE_Y + 0.05, FILM_Z + FILM_H], P);
    }
  }
  return lineSvg(d, { tone: "hi" });
}

// The gauge's needle: 100 at the top mark, and a quarter turn back for every 50 lost.
function needleD(reads, P) {
  const a = ((100 - reads) / 50) * (Math.PI / 2);
  const tip = [DIAL[0] - Math.sin(a) * (DIAL_R - 3.4), DIAL[1] - Math.cos(a) * (DIAL_R - 3.4)];
  const tail = [DIAL[0] + Math.sin(a) * 2.6, DIAL[1] + Math.cos(a) * 2.6];
  return lineOnTop(tail, tip, DIAL_TOP + 0.2, P);
}

const ruleAt = (through) => RULE[0] + (RULE[1] - RULE[0]) * through;
const sayOf = (through, reads) => Math.round(through * 100) + "% wound · gate reads " + Math.round(reads) + "% sound" + (reads < 99.5 ? " · repairing" : "");
