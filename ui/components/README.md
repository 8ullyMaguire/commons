# Estate components

Six web components, vanilla, no build step, no framework. They exist so the same markup produces the same
look in a SvelteKit page, a Go `html/template` and an axum renderer — sharing code where the stacks are the
same, sharing *behaviour and appearance* where they are not.

```
estate-components.js     all six custom elements (171 lines, one file on purpose)
index.html               a demo page that renders every component against the real tokens
```

## What is here, and why these six

| element | attributes | why it is shared |
|---|---|---|
| `<estate-trust-badge level="0..4">` | `level` | A trust ladder drawn six different ways is six different ladders to the reader |
| `<estate-mod-banner tone="warning\|danger\|info">` | `tone`, slot `title` | Moderation must sound like one voice across the estate |
| `<estate-post-card title meta href>` | `title`, `meta`, `href` | The card shell is identical everywhere; only the slotted content differs |
| `<estate-empty-state headline>` | `headline` | Empty states are where products usually drift apart first |
| `<estate-pagination page pages>` | `page`, `pages`; emits `page-change` | Same paging idiom, same event payload |
| `<estate-toast tone dismiss-after>` | `tone`, `dismiss-after`; emits `toast-dismiss` | One transient-message idiom instead of four |

## The rule that keeps this from becoming a second design system

**Zero literal colours or lengths.** Every value is `var(--token)` from `../tokens/tokens.css`. That is
greppable, so it is a reviewable rule rather than a good intention:

```
$ grep -oE '#[0-9a-fA-F]{3,8}\b' estate-components.js | wc -l
0
```

The one fallback that existed (`var(--text, #e6e9ec)`) was removed, because "mostly tokenised" is how a
component library quietly acquires its own palette. Tokens must be loaded; if they are not, the page looks
wrong immediately instead of looking subtly different forever.

Unknown trust levels render as `unverified` (0) rather than inventing a sixth state — a badge that cannot be
trusted to mean one thing is worse than no badge.

## Verified rendering (browser, not a compile)

Loaded `index.html` in a real browser against `tokens.css`:

```
custom elements defined .................. 6/6
elements upgraded with shadow content ... 6/6
badge colours (levels 0-4, then 99) ..... rgb(107,117,128) rgb(138,180,255) rgb(61,214,140)
                                          rgb(199,146,234) rgb(245,165,36) rgb(107,117,128)
level 99 falls back to level 0 .......... yes (last colour == first)
body background from --bg ............... rgb(16,18,20)   (#101214)
body text from --text ................... rgb(230,233,236)
post-card exposes a slot ................ yes
pagination "Page 3 of 12", 2 buttons .... yes
click Next -> page-change detail.page ... 4   (document.title updated to "page 4")
```

Every colour above comes from the token file, which is the point: the components have no palette of their
own to disagree with it.

## Using them

```html
<link rel="stylesheet" href=".../ui/tokens/tokens.css">
<script type="module" src=".../ui/components/estate-components.js"></script>
<estate-trust-badge level="2"></estate-trust-badge>
```

Shadow DOM inherits custom properties from `:root`, so one stylesheet covers every component on the page —
that is why tokens are loaded as a normal stylesheet and not injected per component.

Not here on purpose: icons, fonts, layout grids, and anything that needs a router or state. Those are
application concerns and would turn this into a framework.
