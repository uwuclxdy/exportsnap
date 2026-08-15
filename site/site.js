/* exportsnap site — navbar ink, scroll reveals, dot-grid background.
   No dependencies, no network calls. */

(function () {
  'use strict';

  // ---- theme toggle ----
  const root = document.documentElement;
  const saved = localStorage.getItem('exportsnap-theme');
  if (saved === 'light' || saved === 'dark') root.dataset.theme = saved;

  document.getElementById('theme-toggle')?.addEventListener('click', () => {
    const next = root.dataset.theme === 'light' ? 'dark' : 'light';
    root.dataset.theme = next;
    localStorage.setItem('exportsnap-theme', next);
  });

  // ---- navbar ink, driven by which section is on screen ----
  const nav = document.getElementById('navbar-nav');
  const ink = document.getElementById('navbar-ink');
  const links = [...document.querySelectorAll('.navbar-link[href^="#"]')];

  function positionInk() {
    if (!nav || !ink) return;
    const active = nav.querySelector('.navbar-link.active');
    if (!active) { ink.style.opacity = '0'; return; }
    const nRect = nav.getBoundingClientRect();
    const aRect = active.getBoundingClientRect();
    ink.style.left = (aRect.left - nRect.left) + 'px';
    ink.style.width = aRect.width + 'px';
    ink.style.opacity = '1';
  }

  function setActive(link) {
    links.forEach(l => l.classList.toggle('active', l === link));
    positionInk();
  }

  links.forEach(l => l.addEventListener('click', () => setActive(l)));

  // Track the section under the top of the viewport, so scrolling moves the ink too.
  const sections = links
    .map(l => ({ link: l, el: document.querySelector(l.getAttribute('href')) }))
    .filter(s => s.el);

  if (sections.length && 'IntersectionObserver' in window) {
    const seen = new Set();
    const io = new IntersectionObserver(entries => {
      entries.forEach(e => e.isIntersecting ? seen.add(e.target) : seen.delete(e.target));
      const first = sections.find(s => seen.has(s.el));
      if (first) setActive(first.link);
    }, { rootMargin: '-56px 0px -60% 0px', threshold: 0 });
    sections.forEach(s => io.observe(s.el));
  }

  addEventListener('resize', () => {
    if (!ink) return;
    ink.style.transition = 'none';
    positionInk();
    requestAnimationFrame(() => { ink.style.transition = ''; });
  });
  requestAnimationFrame(positionInk);

  // ---- scroll reveals ----
  const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;

  if (!reduced && 'IntersectionObserver' in window) {
    const targets = [...document.querySelectorAll('[data-reveal]')];
    if (targets.length) {
      targets.forEach((el, i) => {
        const wipe = el.dataset.reveal === 'wipe';
        el.classList.add(wipe ? 'reveal-wipe' : 'reveal');
        if (wipe) el.style.transitionDelay = (i % 6) * 50 + 'ms';
      });
      const io = new IntersectionObserver(entries => {
        entries.forEach(e => {
          if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
        });
      }, { threshold: 0, rootMargin: '0px 0px -10% 0px' });
      targets.forEach(el => io.observe(el));
      setTimeout(() => targets.forEach(el => el.classList.add('in')), 1500);
    }
  }

  // ---- cursor-reactive dot grid ----
  const canvas = document.getElementById('bg-dots');
  if (!canvas) return;
  const ctx = canvas.getContext('2d');
  if (!ctx) return;

  const interactive = matchMedia('(pointer: fine)').matches && !reduced;
  const gap = 40, R = 150;
  const mouse = { x: 0, y: 0, active: false };
  let w = 0, h = 0, dots = [], scheduled = false;

  // Read the two grid colours off the live tokens so the grid follows the theme.
  function ramp() {
    const s = getComputedStyle(root);
    const parse = name => {
      const v = s.getPropertyValue(name).trim();
      const m = /^#([0-9a-f]{6})$/i.exec(v);
      if (!m) return null;
      const n = parseInt(m[1], 16);
      return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
    };
    return {
      base: parse('--line-strong') || [69, 71, 90],
      lit: parse('--accent') || [67, 171, 229],
    };
  }
  let colors = ramp();

  function build() {
    const dpr = Math.min(devicePixelRatio || 1, 2);
    w = innerWidth; h = innerHeight;
    canvas.width = w * dpr; canvas.height = h * dpr;
    canvas.style.width = w + 'px'; canvas.style.height = h + 'px';
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    dots = [];
    const cols = Math.ceil(w / gap) + 1, rows = Math.ceil(h / gap) + 1;
    const ox = (w - (cols - 1) * gap) / 2, oy = (h - (rows - 1) * gap) / 2;
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) dots.push({ bx: ox + x * gap, by: oy + y * gap });
    }
  }

  function draw() {
    ctx.clearRect(0, 0, w, h);
    const on = interactive && mouse.active;
    const shx = on ? (mouse.x - w / 2) / w * 10 : 0;
    const shy = on ? (mouse.y - h / 2) / h * 10 : 0;
    for (const d of dots) {
      const px = d.bx + shx, py = d.by + shy;
      let a = 0.24, r = 1.15, c = colors.base;
      if (on) {
        const dist = Math.hypot(px - mouse.x, py - mouse.y);
        if (dist < R) {
          const f = (1 - dist / R) ** 2;
          a = 0.24 + f * 0.56;
          r = 1 + f * 1.6;
          c = colors.base.map((b, i) => Math.round(b + (colors.lit[i] - b) * f));
        }
      }
      ctx.beginPath();
      ctx.arc(px, py, r, 0, Math.PI * 2);
      ctx.fillStyle = `rgba(${c[0]},${c[1]},${c[2]},${a})`;
      ctx.fill();
    }
  }

  function schedule() {
    if (scheduled) return;
    scheduled = true;
    requestAnimationFrame(() => { scheduled = false; draw(); });
  }

  build(); draw();

  if (interactive) {
    addEventListener('mousemove', e => {
      mouse.x = e.clientX; mouse.y = e.clientY; mouse.active = true; schedule();
    }, { passive: true });
    root.addEventListener('mouseleave', () => { mouse.active = false; schedule(); });
  }
  addEventListener('resize', () => { build(); draw(); });

  // The theme swap changes both grid colours; repaint once it lands.
  new MutationObserver(() => { colors = ramp(); schedule(); })
    .observe(root, { attributes: true, attributeFilter: ['data-theme'] });
})();
