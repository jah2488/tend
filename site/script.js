// tend marketing site - small, focused interactions.
// 1) working-state spinner glyph cycle (mirrors the live TUI: ◐ ◑ ◒ ◓)
// 2) copy-to-clipboard for install commands
// 3) scroll reveal

const SPIN = ['◐', '◓', '◒', '◑']; // ◐ ◓ ◒ ◑

(function spinners() {
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const nodes = document.querySelectorAll('[data-spin]');
  if (reduce || !nodes.length) {
    nodes.forEach((n) => { n.textContent = '●'; }); // static ● on reduced motion
    return;
  }
  let i = 0;
  // stagger by giving each node its own offset
  const offsets = new Map();
  nodes.forEach((n) => { offsets.set(n, nodes.length ? (i++ % SPIN.length) : 0); });
  let t = 0;
  setInterval(() => {
    t++;
    nodes.forEach((n) => { n.textContent = SPIN[(t + offsets.get(n)) % SPIN.length]; });
  }, 130);
})();

(function copyButtons() {
  document.querySelectorAll('.copy-btn').forEach((btn) => {
    btn.addEventListener('click', async () => {
      const text = btn.getAttribute('data-copy') || '';
      const label = btn.querySelector('.copy-label');
      try {
        await navigator.clipboard.writeText(text);
      } catch {
        // fallback for headless / insecure contexts
        const ta = document.createElement('textarea');
        ta.value = text; document.body.appendChild(ta); ta.select();
        try { document.execCommand('copy'); } catch {}
        ta.remove();
      }
      const prev = label ? label.textContent : '';
      btn.classList.add('copied');
      if (label) label.textContent = 'copied ✓';
      setTimeout(() => {
        btn.classList.remove('copied');
        if (label) label.textContent = prev || 'copy';
      }, 1400);
    });
  });
})();

(function reveal() {
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  const items = document.querySelectorAll('.reveal');
  if (reduce || !('IntersectionObserver' in window)) {
    items.forEach((el) => el.classList.add('in'));
    return;
  }
  const io = new IntersectionObserver((entries) => {
    entries.forEach((e) => {
      if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); }
    });
  }, { rootMargin: '0px 0px -8% 0px', threshold: 0.06 });
  items.forEach((el) => io.observe(el));
})();
