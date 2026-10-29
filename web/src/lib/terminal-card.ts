/** Lines drawn as a terminal card, sized to what is drawn: Florilegium's export, for what is
 * said in a clip. Every line of text is a prompt line; the source follows, set apart. */
export const terminalColors = { background: '#1e1e1e', text: '#61d095', source: '#a4aaa6' };
type Glyph = { text: string; width: number; source: boolean };
const SIZE = 18,
  LEADING = 26,
  MARGIN = 14,
  MAX_TEXT_WIDTH = 648;
const MONO = `"SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace`;
const FONT = `400 ${SIZE}px ${MONO}`;

/** Draw `said`, one entry to a line and each wrapped to the card's width, then the source. */
export function drawTerminalCard(canvas: HTMLCanvasElement, said: string[], source = '') {
  const ctx = canvas.getContext('2d')!;
  ctx.font = FONT;
  const segmenter = new Intl.Segmenter(undefined, { granularity: 'grapheme' });
  const glyphsOf = (text: string, source: boolean): Glyph[] => Array.from(segmenter.segment(text), ({ segment }) => ({ text: segment, width: ctx.measureText(segment).width, source }));
  const text = said.map((s) => s.replace(/\r\n?/g, '\n').replace(/\t/g, '    ').trim()).filter(Boolean).join('\n');
  const glyphs = [...glyphsOf(text, false), ...(source ? glyphsOf(`\n\n— ${source}`, true) : [])];

  const lines: Glyph[][] = [];
  let line: Glyph[] = [],
    width = 0;
  const commit = () => {
    while (line.at(-1)?.text === ' ') line.pop();
    lines.push(line);
    line = [];
    width = 0;
  };
  for (const glyph of glyphs) {
    if (glyph.text === '\n') {
      commit();
      continue;
    }
    if (width + glyph.width > MAX_TEXT_WIDTH && line.length) {
      const space = line.map((g) => g.text).lastIndexOf(' ');
      if (space >= 0 && glyph.text !== ' ') {
        const carry = line.splice(space + 1);
        commit();
        line = carry;
        width = line.reduce((sum, g) => sum + g.width, 0);
      } else commit();
      if (glyph.text === ' ') continue;
    }
    line.push(glyph);
    width += glyph.width;
  }
  commit();

  const prefix = ctx.measureText('> ').width;
  const W = Math.ceil(2 * MARGIN + prefix + Math.max(0, ...lines.map((l) => l.reduce((s, g) => s + g.width, 0))));
  const H = 2 * MARGIN + lines.length * LEADING;
  // Bound bitmap memory for unusually long passages without clipping their text.
  const scale = Math.min(3, 16384 / Math.max(W, H), Math.sqrt(24000000 / (W * H)));
  canvas.width = Math.ceil(W * scale);
  canvas.height = Math.ceil(H * scale);
  ctx.setTransform(scale, 0, 0, scale, 0, 0);
  ctx.fillStyle = terminalColors.background;
  ctx.fillRect(0, 0, W, H);
  ctx.textBaseline = 'middle';
  ctx.font = FONT;
  lines.forEach((line, index) => {
    const y = MARGIN + LEADING * (index + 0.5);
    ctx.fillStyle = terminalColors.text;
    ctx.fillText('>', MARGIN, y);
    let x = MARGIN + prefix;
    for (const glyph of line) {
      ctx.fillStyle = glyph.source ? terminalColors.source : terminalColors.text;
      ctx.fillText(glyph.text, x, y);
      x += glyph.width;
    }
  });
}
