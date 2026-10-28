/* Estate web components — Tier 1, no framework, no build step.
 *
 * Vanilla custom elements with shadow DOM so a SvelteKit page, a Go html/template and an axum renderer can
 * all use the same markup and get the same look. Every colour and length comes from the token file
 * (ui/tokens/tokens.css); these components contain ZERO literal hex values, which is the rule that keeps them
 * from becoming a second design system.
 *
 * Usage:
 *   <link rel="stylesheet" href=".../tokens/tokens.css">   (tokens must be on the page: shadow DOM inherits
 *                                                          custom properties from :root)
 *   <script type="module" src=".../components/estate-components.js"></script>
 *   <estate-trust-badge level="2"></estate-trust-badge>
 */

const LEVELS = [
  { name: 'unverified', label: 'Unverified', hint: 'New account, nothing vouched for yet' },
  { name: 'known', label: 'Known', hint: 'Has been around and behaved' },
  { name: 'trusted', label: 'Trusted', hint: 'Long record, no open reports' },
  { name: 'moderator', label: 'Moderator', hint: 'Can act on reports' },
  { name: 'operator', label: 'Operator', hint: 'Runs this instance' },
];

const sheet = new CSSStyleSheet();
sheet.replaceSync(`
  :host { font-family: var(--font-sans, system-ui, sans-serif); color: var(--text); }
  :host([hidden]) { display: none; }
  .box { background: var(--surface); border: 1px solid var(--border); border-radius: var(--radius);
         padding: var(--space-4); }
  .muted { color: var(--text-muted); font-size: var(--size-sm); }
  .badge { display: inline-flex; align-items: center; gap: var(--space-2);
           border-radius: var(--radius-pill); padding: 0.15rem var(--space-3);
           font-size: var(--size-xs); border: 1px solid currentColor; }
  .dot { width: 0.5rem; height: 0.5rem; border-radius: var(--radius-pill); background: currentColor; }
  .banner { display: flex; gap: var(--space-3); align-items: flex-start; border-left: 3px solid var(--warning);
            background: var(--surface-raised); border-radius: var(--radius); padding: var(--space-3) var(--space-4); }
  .banner[data-tone="danger"] { border-left-color: var(--danger); }
  .banner[data-tone="info"] { border-left-color: var(--accent); }
  .card { display: flex; flex-direction: column; gap: var(--space-2); background: var(--surface);
          border: 1px solid var(--border); border-radius: var(--radius); padding: var(--space-4);
          transition: border-color var(--duration-fast) var(--ease); }
  .card:hover { border-color: var(--border-strong); }
  .card h3 { margin: 0; font-size: var(--size-lg); line-height: var(--leading-tight); }
  .empty { text-align: center; padding: var(--space-12) var(--space-4); color: var(--text-muted); }
  .empty strong { display: block; color: var(--text); font-size: var(--size-lg); margin-bottom: var(--space-2); }
  nav { display: flex; gap: var(--space-2); align-items: center; }
  button { font: inherit; color: var(--text); background: var(--surface-raised); border: 1px solid var(--border);
           border-radius: var(--radius-sm); padding: var(--space-2) var(--space-3); cursor: pointer;
           transition: border-color var(--duration-fast) var(--ease); }
  button:hover:not(:disabled) { border-color: var(--border-strong); }
  button:disabled { color: var(--text-faint); cursor: not-allowed; }
  button:focus-visible, a:focus-visible { outline: 2px solid var(--focus); outline-offset: 2px; }
  .toast { display: flex; gap: var(--space-3); align-items: center; background: var(--surface-raised);
           border: 1px solid var(--border); border-radius: var(--radius); box-shadow: var(--shadow-2);
           padding: var(--space-3) var(--space-4); }
  .toast[data-tone="success"] { border-left: 3px solid var(--success); }
  .toast[data-tone="danger"] { border-left: 3px solid var(--danger); }
  @media (prefers-reduced-motion: reduce) { * { transition: none !important; } }
`);

/** <estate-trust-badge level="0..4"> — the same ladder on every site, by construction. */
class EstateTrustBadge extends HTMLElement {
  static get observedAttributes() { return ['level']; }
  constructor() { super(); this.attachShadow({ mode: 'open' }).adoptedStyleSheets = [sheet]; }
  connectedCallback() { this.render(); }
  attributeChangedCallback() { if (this.shadowRoot.firstChild) this.render(); }
  render() {
    const raw = Number.parseInt(this.getAttribute('level') ?? '0', 10);
    // An unknown level renders as unverified rather than inventing a sixth state: a badge that cannot be
    // trusted to mean one thing is worse than no badge.
    const level = Number.isInteger(raw) && raw >= 0 && raw <= 4 ? raw : 0;
    const spec = LEVELS[level];
    this.setAttribute('aria-label', `Trust level: ${spec.label}`);
    this.setAttribute('title', spec.hint);
    this.shadowRoot.innerHTML =
      `<span class="badge" style="color: var(--trust-${level})" part="badge">
         <span class="dot" aria-hidden="true"></span><span>${spec.label}</span>
       </span>`;
  }
}

/** <estate-mod-banner tone="warning|danger|info"> — one moderation voice across the estate. */
class EstateModBanner extends HTMLElement {
  constructor() { super(); this.attachShadow({ mode: 'open' }).adoptedStyleSheets = [sheet]; }
  connectedCallback() {
    const tone = this.getAttribute('tone') ?? 'warning';
    this.shadowRoot.innerHTML =
      `<div class="banner" data-tone="${tone}" role="status">
         <strong style="color: var(--${tone === 'danger' ? 'danger' : tone === 'info' ? 'accent' : 'warning'})">
           <slot name="title">Moderation</slot>
         </strong>
         <span class="muted"><slot></slot></span>
       </div>`;
  }
}

/** <estate-post-card title="…" meta="…" href="…">body…</estate-post-card> — shell only; content is slotted. */
class EstatePostCard extends HTMLElement {
  constructor() { super(); this.attachShadow({ mode: 'open' }).adoptedStyleSheets = [sheet]; }
  connectedCallback() {
    const title = this.getAttribute('title') ?? '';
    const meta = this.getAttribute('meta') ?? '';
    const href = this.getAttribute('href');
    const heading = href
      ? `<h3><a href="${href}" style="color:inherit;text-decoration:none">${title}</a></h3>`
      : `<h3>${title}</h3>`;
    this.shadowRoot.innerHTML =
      `<article class="card" part="card">${heading}
         ${meta ? `<span class="muted">${meta}</span>` : ''}
         <slot></slot>
       </article>`;
  }
}

/** <estate-empty-state headline="…">what the reader can do next</estate-empty-state> */
class EstateEmptyState extends HTMLElement {
  constructor() { super(); this.attachShadow({ mode: 'open' }).adoptedStyleSheets = [sheet]; }
  connectedCallback() {
    this.shadowRoot.innerHTML =
      `<div class="empty"><strong>${this.getAttribute('headline') ?? 'Nothing here yet'}</strong>
         <slot></slot></div>`;
  }
}

/** <estate-pagination page="3" pages="12" base="?page="> — emits `page-change`. */
class EstatePagination extends HTMLElement {
  static get observedAttributes() { return ['page', 'pages']; }
  constructor() { super(); this.attachShadow({ mode: 'open' }).adoptedStyleSheets = [sheet]; }
  connectedCallback() { this.render(); }
  attributeChangedCallback() { if (this.shadowRoot.firstChild) this.render(); }
  render() {
    const page = Math.max(1, Number.parseInt(this.getAttribute('page') ?? '1', 10) || 1);
    const pages = Math.max(1, Number.parseInt(this.getAttribute('pages') ?? '1', 10) || 1);
    this.shadowRoot.innerHTML =
      `<nav aria-label="Pagination">
         <button data-step="-1" ${page <= 1 ? 'disabled' : ''} aria-label="Previous page">Prev</button>
         <span class="muted" aria-live="polite">Page ${page} of ${pages}</span>
         <button data-step="1" ${page >= pages ? 'disabled' : ''} aria-label="Next page">Next</button>
       </nav>`;
    this.shadowRoot.querySelectorAll('button').forEach((b) =>
      b.addEventListener('click', () => this.dispatchEvent(new CustomEvent('page-change', {
        detail: { page: page + Number(b.dataset.step) }, bubbles: true, composed: true,
      }))));
  }
}

/** <estate-toast tone="success|danger" dismiss-after="4000"> — one transient message idiom. */
class EstateToast extends HTMLElement {
  constructor() { super(); this.attachShadow({ mode: 'open' }).adoptedStyleSheets = [sheet]; }
  connectedCallback() {
    const tone = this.getAttribute('tone') ?? 'success';
    this.shadowRoot.innerHTML =
      `<div class="toast" data-tone="${tone}" role="status" aria-live="polite"><slot></slot></div>`;
    const ms = Number.parseInt(this.getAttribute('dismiss-after') ?? '0', 10);
    if (ms > 0) setTimeout(() => this.dispatchEvent(new CustomEvent('toast-dismiss', { bubbles: true })), ms);
  }
}

const ELEMENTS = {
  'estate-trust-badge': EstateTrustBadge,
  'estate-mod-banner': EstateModBanner,
  'estate-post-card': EstatePostCard,
  'estate-empty-state': EstateEmptyState,
  'estate-pagination': EstatePagination,
  'estate-toast': EstateToast,
};

for (const [name, ctor] of Object.entries(ELEMENTS)) {
  if (!customElements.get(name)) customElements.define(name, ctor);
}

export { EstateTrustBadge, EstateModBanner, EstatePostCard, EstateEmptyState, EstatePagination, EstateToast, LEVELS };
