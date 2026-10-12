// Build the figure on the site's home page: a film bench that stands for what Spool does.
//
//   node tools/figure/build.mjs                  write it into docs/index.html and docs/assets
//   node tools/figure/build.mjs --preview FILE   also write a page that shows only the figure
//                                                (add --light for it on paper)
//
// The bench: film leaves a feed reel, runs through a gate under a gauge, and winds onto a
// take-up reel, with a viewer behind that shows what has been wound. One film, one path,
// one hand on it. The reels' sizes, how fast each turns and what the gauge reads all come
// from the model in shared.js, which the page runs too.
//
// It is drawn with the kit of Anatomy (kit/iso-kit.mjs, MIT): solid parts in one isometric
// view, painted back to front.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import * as k from "./kit/iso-kit.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const DOCS = join(HERE, "..", "..", "docs");
const flag = (name) => { const i = process.argv.indexOf(name); return i < 0 ? null : process.argv[i + 1]; };

// The shared source, run here with the kit's functions in reach.
const SHARED = readFileSync(join(HERE, "shared.js"), "utf8");
const NAMES = ["HUB", "FULL", "PITCH", "LENGTH", "radiusOf", "wrapsOf", "shareAt", "DAMAGE", "soundAt", "SUPPLY", "TAKEUP", "GUIDES", "GUIDE_R", "LINE_Y", "PACK_Z", "PACK_H", "HUB_TOP", "FILM_Z", "FILM_H", "DIAL", "DIAL_R", "DIAL_TOP", "RULE", "ribbon", "runSvg", "packSvg", "hubSvg", "damageSvg", "needleD", "ruleAt", "sayOf"];
const m = new Function(...Object.keys(k), `${SHARED}\nreturn { ${NAMES.join(", ")} };`)(...Object.values(k));

const WIDTH = 760, HEIGHT = 404;
const PLATE = { x: -152, y: -68, w: 304, d: 136, r: 9 };
const PLATE_H = 11, FOOT_H = 5;
const PLATTER_R = 46;
const BLOCK = { x: -14, y: 20, w: 28, d: 26, r: 2.5 };
const BLOCK_H = 16;
const SHOE = { x: -7, y: m.LINE_Y + 1.2, w: 14, d: 5, r: 1.4 };
const VIEWER = { x: -25, y: -60, w: 50, d: 15, r: 2.2 };
const VIEWER_Z = 21, VIEWER_H = 35;
const COUNTER = { x: -142, y: 38, w: 30, d: 19, r: 2 };
const STRIP = { x: -106, y: 58, w: 152, d: 6.4, r: 1 };
const LABEL = { x: 56, y: 55, w: 44, d: 9, r: 1 };
const KNOB = [128, 49];
const START = 0;

const P = k.fitProjection([...k.boxCorners(PLATE, -PLATE_H - FOOT_H, 0), [VIEWER.x, VIEWER.y, VIEWER_Z + VIEWER_H + 3], [VIEWER.x + VIEWER.w, VIEWER.y, VIEWER_Z + VIEWER_H + 3]], WIDTH, HEIGHT, { pad: 12 });

const at = (point) => k.iso(point, P);
const S = (paths, style) => k.solidSvg(paths, style);
const L = (d, style) => k.lineSvg(d, style);
const D = (points, style) => k.dotsSvg(points, style);
const byDepth = (points) => [...points].sort((a, b) => k.depthOf([a[0], a[1], 0], P) - k.depthOf([b[0], b[1], 0], P));
const part = (name, body) => `<g data-part="${name}">${body}</g>`;

function deck() {
  const out = [`<path class="iso-halo" d="${k.haloOf(PLATE, -PLATE_H - FOOT_H, PLATE_H, P)}"/>`];
  for (const [x, y] of byDepth(k.corners(PLATE, 22))) {
    out.push(S(k.cylinder(x, y, 7.4, -PLATE_H - FOOT_H, 1.6, P, 28), { tone: "lo" }));
    out.push(S(k.cylinder(x, y, 5.6, -PLATE_H - FOOT_H + 1.6, FOOT_H - 1.6, P, 28), { tone: "mid" }));
  }
  out.push(S(k.slabOf(PLATE, -PLATE_H, PLATE_H, P, 8, 1.6), { tone: "mid", inner: L(k.sideSeam(PLATE, -3.2, P, 8), { tone: "faint" }) }));
  out.push(L(k.planOutline(k.insetPlan(PLATE, 5), 0, P), { tone: "lo" }));
  const screws = k.corners(PLATE, 11);
  out.push(L(screws.map(([x, y]) => k.ring(x, y, 2.3, 0, P, 16)).join(""), { tone: "lo" }));
  out.push(D(screws.map(([x, y]) => at([x, y, 0])), { size: 0.55 }));

  // The rule along the front, which a rider is moved along as the film goes through.
  out.push(S(k.slabOf(STRIP, 0, 0.7, P, 3), { tone: "lo", crease: "none" }));
  const ticks = k.topTicks(STRIP.x + 4, STRIP.x + STRIP.w - 4, 3.5, 5, STRIP.y + 0.5, 0.7, [1.8, 3.4], P);
  out.push(L(ticks.minor, { tone: "lo" }), L(ticks.major, { tone: "mid" }));
  out.push(D([at([STRIP.x + 1.8, STRIP.y + 3.2, 0.7]), at([STRIP.x + STRIP.w - 1.8, STRIP.y + 3.2, 0.7])], { size: 0.45 }));

  // The maker's plate.
  out.push(S(k.slabOf(LABEL, 0, 0.8, P, 4), { tone: "lo", crease: "none" }));
  out.push(L(k.planOutline(k.insetPlan(LABEL, 1.5), 0.8, P, 3), { tone: "faint" }));
  out.push(L([[26, 3.2], [34, 5], [18, 6.8]].map(([long, y]) => k.lineOnTop([LABEL.x + 5, LABEL.y + y], [LABEL.x + 5 + long, LABEL.y + y], 0.8, P)).join(""), { tone: "lo", free: true }));
  out.push(D([at([LABEL.x + 2.4, LABEL.y + 4.5, 0.8]), at([LABEL.x + LABEL.w - 2.4, LABEL.y + 4.5, 0.8])], { size: 0.45 }));

  // The lead from the lamp in the gate to the viewer, clipped to the deck on its way.
  const lead = [[0, -40], [0, BLOCK.y]];
  out.push(k.wireSvg(at([...lead[0], 0]), at([...lead[1], 0]), { tone: "mid", size: 1.1 }));
  out.push(D([-24, -8, 8].map((y) => at([0, y, 0])), { size: 0.6, tone: "mid" }));
  return out.join("");
}

function viewer() {
  const out = [];
  const foot = { x: -11, y: -57, w: 22, d: 17, r: 2.5 };
  out.push(S(k.slabOf(foot, 0, 3, P, 4, 0.5), { tone: "mid" }));
  out.push(D(k.corners(foot, 3).map(([x, y]) => at([x, y, 3])), { size: 0.4 }));
  out.push(S(k.cylinder(0, -52, 5.2, 3, 3, P, 24), { tone: "mid" }));
  out.push(S(k.cylinder(0, -52, 3.4, 6, VIEWER_Z - 6, P, 24), { tone: "mid", crease: "none", inner: L(k.sideArc(0, -52, 3.4, 13, P, 14), { tone: "faint" }) }));
  const top = VIEWER_Z + VIEWER_H;
  out.push(S(k.slabOf(VIEWER, VIEWER_Z, VIEWER_H, P, 5, 0.8), { tone: "mid", inner: L(k.sideSeam(VIEWER, VIEWER_Z + 4, P, 5), { tone: "faint" }) }));
  // The glass, on the face that looks at us, with a hood over it.
  const face = VIEWER.y + VIEWER.d;
  const glass = [[VIEWER.x + 5, VIEWER_Z + 8], [VIEWER.x + VIEWER.w - 5, VIEWER_Z + 8], [VIEWER.x + VIEWER.w - 5, top - 5], [VIEWER.x + 5, top - 5]];
  const onFace = (points) => k.pathOf(points.map(([x, z]) => at([x, face, z])), true);
  out.push(`<path class="iso-fill bench-glass" d="${onFace(glass)}"/>`, L(onFace(glass), { tone: "lo" }));
  // What it shows once film has come through: a horizon and a sun.
  const sun = k.sideRing([6, face, VIEWER_Z + 24], 4.2, "y", P, 24);
  const land = k.pathOf([[VIEWER.x + 5, VIEWER_Z + 17], [-8, VIEWER_Z + 21], [3, VIEWER_Z + 16.5], [12, VIEWER_Z + 19.5], [VIEWER.x + VIEWER.w - 5, VIEWER_Z + 15.5]].map(([x, z]) => at([x, face, z])));
  out.push(`<g data-part="picture" style="opacity:0">${L(sun, { tone: "hi" })}${L(land, { tone: "hi" })}</g>`);
  out.push(D([[VIEWER.x + 2.4, VIEWER_Z + 2.6], [VIEWER.x + VIEWER.w - 2.4, VIEWER_Z + 2.6], [VIEWER.x + 2.4, top - 2.4], [VIEWER.x + VIEWER.w - 2.4, top - 2.4]].map(([x, z]) => at([x, face, z])), { size: 0.4 }));
  out.push(S(k.slabOf({ x: VIEWER.x - 1.5, y: VIEWER.y - 1, w: VIEWER.w + 3, d: VIEWER.d + 7, r: 2 }, top, 1.6, P, 4, 0.4), { tone: "mid" }));
  return out.join("");
}

// A reel's plate and what stands on it. The film on it and the marks that turn are drawn
// by the model, in the slots named here.
function reel(name, centre, share, turn) {
  const [x, y] = centre;
  const out = [];
  out.push(S(k.cylinder(x, y, 15, 0, 3, P, 40, 0.5), { tone: "mid" }));
  out.push(S(k.cylinder(x, y, PLATTER_R, 3, 3, P, 80, 0.9), { tone: "mid", crease: "lo" }));
  // Degree marks hang from a ring just inside the rim.
  out.push(L(k.ring(x, y, PLATTER_R - 1.6, 6, P, 96), { tone: "lo" }));
  const ticks = k.radialTicks(x, y, PLATTER_R - 1.6, 72, 6, 6, [1.3, 2.8], P);
  out.push(L(ticks.minor, { tone: "lo" }), L(ticks.major, { tone: "mid" }));
  out.push(part(`${name}-pack`, m.packSvg(centre, share, P)));
  out.push(S(k.cylinder(x, y, 10, m.PACK_Z, m.HUB_TOP - m.PACK_Z, P, 40, 0.5), { tone: "mid" }));
  out.push(part(`${name}-hub`, m.hubSvg(centre, turn, P)));
  out.push(S(k.cylinder(x, y, 4.4, m.HUB_TOP, 2.6, P, 24), { tone: "mid" }));
  out.push(L(k.knurl(x, y, 4.4, m.HUB_TOP, m.HUB_TOP + 2.6, 22, P), { tone: "lo" }));
  out.push(S(k.cylinder(x, y, 1.9, m.HUB_TOP + 2.6, 3, P, 16), { tone: "mid", crease: "none" }));
  return out.join("");
}

function guide([x, y]) {
  const out = [];
  out.push(S(k.cylinder(x, y, 6.2, 0, 1.6, P, 24), { tone: "mid" }));
  out.push(S(k.cylinder(x, y, m.GUIDE_R, 1.6, 13.6, P, 28, 0.4), { tone: "mid", inner: L(k.sideArc(x, y, m.GUIDE_R, 5.4, P, 16) + k.sideArc(x, y, m.GUIDE_R, 13.2, P, 16), { tone: "faint" }) }));
  out.push(L(k.ring(x, y, 1.5, 15.2, P, 12), { tone: "lo" }));
  out.push(D([at([x, y, 15.2])], { size: 0.5 }));
  return out.join("");
}

// The gate's block, behind the film: the lamp house with the gauge on top of it.
function gate() {
  const out = [];
  const [x, y] = m.DIAL;
  out.push(S(k.slabOf(BLOCK, 0, BLOCK_H, P, 5, 0.8), { tone: "mid", inner: L(k.sideSeam(BLOCK, 3, P, 5), { tone: "faint" }) }));
  out.push(D(k.corners(BLOCK, 2.6).map(([px, py]) => at([px, py, BLOCK_H])), { size: 0.4 }));
  // The lamp's vents, on the side that faces right.
  const side = BLOCK.x + BLOCK.w;
  out.push(L([7, 9.5, 12].map((z) => k.pathOf([[side, BLOCK.y + 5, z], [side, BLOCK.y + 15, z], [side, BLOCK.y + 15, z + 1], [side, BLOCK.y + 5, z + 1]].map(at), true)).join(""), { tone: "lo" }));
  out.push(S(k.cylinder(x, y, m.DIAL_R, BLOCK_H, m.DIAL_TOP - BLOCK_H, P, 56, 0.7), { tone: "hi", crease: "lo", inner: L(k.sideArc(x, y, m.DIAL_R, m.DIAL_TOP - 2.6, P, 32), { tone: "faint" }) }));
  out.push(L(k.knurl(x, y, m.DIAL_R, m.DIAL_TOP - 2.6, m.DIAL_TOP, 60, P, 0.3), { tone: "lo" }));
  out.push(L(k.ring(x, y, m.DIAL_R - 2.4, m.DIAL_TOP, P, 48), { tone: "lo" }));
  // Marks from full, at the top, back to half: the needle never goes further.
  const ticks = k.radialTicks(x, y, m.DIAL_R - 2.4, 10, 5, m.DIAL_TOP, [1.2, 2.4], P, -Math.PI / 2, Math.PI / 2);
  out.push(L(ticks.minor, { tone: "lo" }), L(ticks.major, { tone: "mid" }));
  out.push(L(k.ring(x, y, 1.3, m.DIAL_TOP, P, 14), { tone: "mid" }));
  out.push(`<path data-part="needle" class="iso-line" data-tone="lit" data-free="start" d="${m.needleD(100, P)}" style="stroke-width:1.1"/>`);
  // The feeler that rides on the film, from the gauge's side of the block.
  out.push(S(k.slabOf({ x: -2.2, y: BLOCK.y + BLOCK.d, w: 4.4, d: m.LINE_Y - BLOCK.y - BLOCK.d - 0.5, r: 0.8 }, 8.4, 2.6, P, 2), { tone: "mid", crease: "none" }));
  return out.join("");
}

// The shoe that holds the film to the gate, in front of it.
function shoe() {
  const out = [];
  out.push(S(k.slabOf(SHOE, 0, 11, P, 3, 0.5), { tone: "mid" }));
  out.push(D([at([SHOE.x + 2.4, SHOE.y + 2.5, 11]), at([SHOE.x + SHOE.w - 2.4, SHOE.y + 2.5, 11])], { size: 0.4 }));
  const screw = [0, SHOE.y + SHOE.d + 2.6];
  out.push(S(k.slabOf({ x: -1.3, y: SHOE.y + SHOE.d - 0.4, w: 2.6, d: 2, r: 0.7 }, 4, 3, P, 2), { tone: "mid", crease: "none" }));
  out.push(S(k.cylinder(screw[0], screw[1], 2.5, 3, 5, P, 18), { tone: "mid" }));
  out.push(L(k.knurl(screw[0], screw[1], 2.5, 3, 8, 16, P), { tone: "lo" }));
  return out.join("");
}

function front() {
  const out = [];
  // The counter, with three windows and a wheel to set it by.
  out.push(S(k.slabOf(COUNTER, 0, 9, P, 4, 0.7), { tone: "mid", inner: L(k.sideSeam(COUNTER, 2.4, P, 4), { tone: "faint" }) }));
  out.push(L([0, 1, 2].map((n) => k.planOutline({ x: COUNTER.x + 4 + n * 7.6, y: COUNTER.y + 5, w: 6, d: 8, r: 0.8 }, 9, P, 2)).join(""), { tone: "lo" }));
  out.push(D([at([COUNTER.x + 2.2, COUNTER.y + 2.2, 9]), at([COUNTER.x + COUNTER.w - 2.2, COUNTER.y + COUNTER.d - 2.2, 9])], { size: 0.4 }));
  out.push(S(k.cylinder(COUNTER.x + COUNTER.w + 2.6, COUNTER.y + 9.5, 2.6, 2, 5, P, 18), { tone: "mid" }));
  out.push(L(k.knurl(COUNTER.x + COUNTER.w + 2.6, COUNTER.y + 9.5, 2.6, 2, 7, 16, P), { tone: "lo" }));

  // The rider on the rule.
  const rider = { x: -3, y: STRIP.y - 1.6, w: 6, d: STRIP.d + 3.2, r: 1.2 };
  out.push(`<g data-part="rider" transform="${k.translateAlong(P, [m.ruleAt(START), 0, 0])}">${S(k.slabOf(rider, 0.7, 3.2, P, 3, 0.4), { tone: "hi" })}${L(k.lineOnTop([0, rider.y], [0, rider.y + rider.d], 3.9, P), { tone: "hi" })}</g>`);

  // The one control: a knob for how fast, and a switch.
  const [x, y] = KNOB;
  out.push(S(k.cylinder(x, y, 9.4, 0, 1.4, P, 36), { tone: "mid" }));
  out.push(L(k.ring(x, y, 9, 1.4, P, 40), { tone: "lo" }));
  const dial = k.radialTicks(x, y, 9, 20, 5, 1.4, [1, 1.9], P);
  out.push(L(dial.minor, { tone: "lo" }), L(dial.major, { tone: "mid" }));
  out.push(S(k.cylinder(x, y, 6.2, 1.4, 5.6, P, 36, 0.5), { tone: "mid" }));
  out.push(L(k.knurl(x, y, 6.2, 1.4, 7, 34, P), { tone: "lo" }));
  out.push(L(k.lineOnTop([x, y], [x + 4.4, y - 4.4], 7, P), { tone: "hi" }));
  out.push(D([at([x, y, 7])], { size: 0.5, tone: "hi" }));
  const sw = { x: 104, y: 42, w: 9, d: 13, r: 1.5 };
  out.push(S(k.slabOf(sw, 0, 2.4, P, 3, 0.4), { tone: "mid" }));
  out.push(D([at([sw.x + 4.5, sw.y + 2.2, 2.4]), at([sw.x + 4.5, sw.y + sw.d - 2.2, 2.4])], { size: 0.4 }));
  out.push(S(k.cylinder(sw.x + 4.5, sw.y + 7.4, 1.5, 2.4, 6, P, 14), { tone: "mid" }));
  return out.join("");
}

const share = m.shareAt(START);
const straight = m.ribbon([[m.GUIDES[0][0], m.LINE_Y], [m.GUIDES[1][0], m.LINE_Y]], P);
const LABEL_TEXT = "A film bench. Film leaves a feed reel on the left, bends round a guide roller, runs straight through a gate under a dial gauge, bends round a second roller and winds onto a take-up reel on the right. A small viewer stands behind. As the film goes through, the feed reel shrinks and turns faster, the take-up reel grows and turns slower, and the gauge dips when a damaged stretch reaches the gate.";

const svg = k.figureSvg({
  width: WIDTH,
  height: HEIGHT,
  label: LABEL_TEXT,
  body: [
    deck(),
    viewer(),
    reel("supply", m.SUPPLY, 1 - share, m.wrapsOf(1 - share) * Math.PI * 2),
    gate(),
    guide(m.GUIDES[0]),
    part("run-in", m.runSvg(-1, 1 - share, P)),
    reel("takeup", m.TAKEUP, share, -m.wrapsOf(share) * Math.PI * 2),
    guide(m.GUIDES[1]),
    part("run-out", m.runSvg(1, share, P)),
    straight,
    part("damage", m.damageSvg(START, P)),
    shoe(),
    front(),
  ],
});

// What runs it on the page. The film goes through by itself until someone takes hold.
const LIVE = `
${SHARED}
const P = ${JSON.stringify(P)};
for (const stage of document.querySelectorAll(".bench")) {
  const svg = stage.querySelector("svg");
  const find = (name) => svg.querySelector('[data-part="' + name + '"]');
  const parts = { supplyPack: find("supply-pack"), supplyHub: find("supply-hub"), takeupPack: find("takeup-pack"), takeupHub: find("takeup-hub"), runIn: find("run-in"), runOut: find("run-out"), damage: find("damage"), needle: find("needle"), rider: find("rider"), picture: find("picture") };
  const readout = stage.closest(".iso-plate")?.querySelector("[data-readout]");
  const still = matchMedia("(prefers-reduced-motion: reduce)");
  const clamp = (value, low, high) => Math.min(high, Math.max(low, value));
  // One pass: wind through slowly, rest, wind back fast, rest.
  const PASS = 15, REST = 1.6, BACK = 2.6, LOOP = PASS + REST + BACK + REST;
  const ease = (t) => t * t * (3 - 2 * t);
  const tourAt = (t) => { const at = t % LOOP; return at < PASS ? at / PASS : at < PASS + REST ? 1 : at < PASS + REST + BACK ? 1 - ease((at - PASS - REST) / BACK) : 0; };
  let through = ${START}, target = through, needle = { at: 100, rate: 0 }, drawn = -1;
  let frame = 0, last = 0, clock = 0, touring = !still.matches, visible = false, idle = 0;
  function draw() {
    const share = shareAt(through), reads = soundAt(through);
    if (Math.abs(through - drawn) > 0.0004) {
      drawn = through;
      parts.supplyPack.innerHTML = packSvg(SUPPLY, 1 - share, P);
      parts.takeupPack.innerHTML = packSvg(TAKEUP, share, P);
      parts.supplyHub.innerHTML = hubSvg(SUPPLY, wrapsOf(1 - share) * Math.PI * 2, P);
      parts.takeupHub.innerHTML = hubSvg(TAKEUP, -wrapsOf(share) * Math.PI * 2, P);
      parts.runIn.innerHTML = runSvg(-1, 1 - share, P);
      parts.runOut.innerHTML = runSvg(1, share, P);
      parts.damage.innerHTML = damageSvg(through, P);
      parts.rider.setAttribute("transform", translateAlong(P, [ruleAt(through), 0, 0]));
      parts.picture.style.opacity = String(clamp(through * 12, 0, 1));
      const text = sayOf(through, reads);
      if (readout && readout.textContent !== text) readout.textContent = text;
      stage.setAttribute("aria-valuenow", String(Math.round(through * 100)));
      stage.setAttribute("aria-valuetext", text);
    }
    parts.needle.setAttribute("d", needleD(needle.at, P));
  }
  function tick(now) {
    const dt = last ? Math.min((now - last) / 1000, 1 / 30) : 1 / 60;
    last = now;
    const calm = still.matches;
    if (touring) { clock += dt; target = tourAt(clock); through = target; }
    else through = calm ? target : through + (target - through) * (1 - Math.exp(-dt / 0.22));
    if (Math.abs(target - through) < 0.0004) through = target;
    const want = soundAt(through), stiff = 260, drag = calm ? 2 * Math.sqrt(stiff) : 20;
    for (let i = 0; i < 4; i++) { needle.rate += ((want - needle.at) * stiff - needle.rate * drag) * dt / 4; needle.at += needle.rate * dt / 4; }
    if (Math.abs(want - needle.at) < 0.02 && Math.abs(needle.rate) < 0.02) { needle.at = want; needle.rate = 0; }
    draw();
    const settled = !touring && through === target && needle.rate === 0;
    frame = visible && !settled ? requestAnimationFrame(tick) : 0;
  }
  const run = () => { if (!frame && visible) { last = 0; frame = requestAnimationFrame(tick); } };
  // Taking hold stops the tour; it starts again from where the film was left.
  const hold = (next) => {
    touring = false; target = clamp(next, 0, 1);
    clearTimeout(idle);
    idle = setTimeout(() => { if (still.matches) return; clock = through * PASS; touring = true; run(); }, 3200);
    run();
  };
  new IntersectionObserver((entries) => { visible = entries[entries.length - 1].isIntersecting; if (visible) run(); }, { rootMargin: "120px 0px" }).observe(stage);
  const reach = (event) => { const box = svg.getBoundingClientRect(); hold(((event.clientX - box.left) / box.width - 0.14) / 0.72); };
  stage.addEventListener("pointermove", (event) => { if (event.pointerType !== "touch" || event.buttons) reach(event); });
  stage.addEventListener("pointerdown", reach);
  stage.addEventListener("keydown", (event) => {
    const step = event.shiftKey ? 0.1 : 0.02;
    const to = event.key === "ArrowRight" || event.key === "ArrowUp" ? target + step : event.key === "ArrowLeft" || event.key === "ArrowDown" ? target - step : event.key === "Home" ? 0 : event.key === "End" ? 1 : null;
    if (to === null) return;
    event.preventDefault();
    hold(to);
  });
  draw();
}
`;

const kitSource = readFileSync(join(HERE, "kit", "iso-kit.mjs"), "utf8").replace(/^export /gm, "");
const script = `// Made by tools/figure/build.mjs; edit that and build again.\n// The first part is the drawing kit of Anatomy (github.com/wheresryan22/anatomy), MIT, (c) 2026 Ryan.\n${kitSource}\n${LIVE}`;

const card = (extra = "") =>
  k.plateHtml({
    fig: "Fig 1",
    title: "One film, one path",
    hint: "Move across it to wind",
    readout: m.sayOf(START, 100),
    body: `<div class="iso-stage bench" tabindex="0" role="slider" aria-label="A film bench: the arrow keys wind the film through" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0"${extra}>${svg}</div>`,
  });

// On the site the figure takes the page's own colours, dark or on paper as the reader has it.
const light = k.ISO_CSS.match(/\.iso\[data-theme="light"\]\{([^}]*)\}/)[1];
const css = `/* Made by tools/figure/build.mjs. The first part is the style sheet of Anatomy's drawing kit (MIT). */
${k.ISO_CSS}
@media (prefers-color-scheme: light){.iso:not([data-theme="dark"]){${light}}}
.bench{outline:none;touch-action:pan-y;cursor:ew-resize}
.bench:focus-visible{outline:1px solid var(--anatomy-mid);outline-offset:6px}
.bench-glass{fill:var(--anatomy-shade-0)}
`;

writeFileSync(join(DOCS, "assets", "bench.js"), script);
writeFileSync(join(DOCS, "assets", "bench.css"), css);
const index = join(DOCS, "index.html");
const page = readFileSync(index, "utf8");
const marks = /(<!-- figure: made by tools\/figure\/build\.mjs -->)[\s\S]*?(<!-- \/figure -->)/;
if (marks.test(page)) writeFileSync(index, page.replace(marks, (_, open, close) => `${open}\n${card()}\n${close}`));
else console.log("docs/index.html has no place marked for the figure; wrote only its script and styles");

const preview = flag("--preview");
if (preview) {
  const theme = process.argv.includes("--light") ? "light" : "dark";
  writeFileSync(preview, k.pageHtml({ title: "Spool: the bench", theme, width: 860, body: `<style>.bench{outline:none;cursor:ew-resize}.bench-glass{fill:var(--anatomy-shade-0)}</style>${card()}`, script: kitSource + LIVE }));
  console.log("wrote", preview);
}
console.log(`figure: ${(svg.match(/class="iso-solid/g) || []).length} solids, ${(svg.match(/<path/g) || []).length} paths, ${Math.round(svg.length / 1024)} KB of drawing, ${Math.round(script.length / 1024)} KB of script`);
