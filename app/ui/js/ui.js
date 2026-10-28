// Small DOM helpers shared by the app: element builder, icons, formatting,
// menus, popovers, dialogs and the toast. Text is only ever set with
// textContent; the icon markup below is the only innerHTML, and it is constant.
"use strict";
(function (Fatstick) {
  const ICONS = {
    play: '<svg viewBox="0 0 24 24"><path d="M8 5.5v13a1 1 0 0 0 1.5.86l10.4-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5z"/></svg>',
    pause: '<svg viewBox="0 0 24 24"><rect x="6.5" y="5" width="4" height="14" rx="1"/><rect x="13.5" y="5" width="4" height="14" rx="1"/></svg>',
    prev: '<svg viewBox="0 0 24 24"><rect x="5" y="5.5" width="2.6" height="13" rx="1"/><path d="M19 6.4v11.2a.9.9 0 0 1-1.38.76L9.1 12.76a.9.9 0 0 1 0-1.52l8.52-5.6A.9.9 0 0 1 19 6.4z"/></svg>',
    next: '<svg viewBox="0 0 24 24"><rect x="16.4" y="5.5" width="2.6" height="13" rx="1"/><path d="M5 6.4v11.2a.9.9 0 0 0 1.38.76l8.52-5.6a.9.9 0 0 0 0-1.52L6.38 5.64A.9.9 0 0 0 5 6.4z"/></svg>',
    plus: '<svg viewBox="0 0 24 24"><path d="M11 5h2v6h6v2h-6v6h-2v-6H5v-2h6z"/></svg>',
    up: '<svg viewBox="0 0 24 24"><path d="M12 7.5l6 6-1.4 1.4-4.6-4.6-4.6 4.6L6 13.5z"/></svg>',
    down: '<svg viewBox="0 0 24 24"><path d="M12 16.5l-6-6 1.4-1.4 4.6 4.6 4.6-4.6 1.4 1.4z"/></svg>',
    close: '<svg viewBox="0 0 24 24"><path d="M6.4 5L12 10.6 17.6 5 19 6.4 13.4 12l5.6 5.6-1.4 1.4-5.6-5.6L6.4 19 5 17.6l5.6-5.6L5 6.4z"/></svg>',
    caret: '<svg viewBox="0 0 24 24"><path d="M7 10l5 5 5-5z"/></svg>',
    volume: '<svg viewBox="0 0 24 24"><path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z"/><path d="M15 8.5a5 5 0 0 1 0 7l-1.2-1.2a3.3 3.3 0 0 0 0-4.6zM17.6 6a8.5 8.5 0 0 1 0 12l-1.2-1.2a6.8 6.8 0 0 0 0-9.6z"/></svg>',
    check: '<svg viewBox="0 0 24 24"><path d="M9.5 16.2L5.3 12l-1.4 1.4 5.6 5.6L20.1 8.4 18.7 7z"/></svg>',
    search: '<svg viewBox="0 0 24 24"><path fill-rule="evenodd" d="M10.5 4a6.5 6.5 0 1 0 0 13a6.5 6.5 0 1 0 0-13zM10.5 6a4.5 4.5 0 1 1 0 9a4.5 4.5 0 1 1 0-9z"/><path d="M14.2 15.6l1.4-1.4 4.8 4.8-1.4 1.4z"/></svg>',
  };

  function icon(name) {
    const s = document.createElement("span");
    s.className = "ic";
    s.setAttribute("aria-hidden", "true");
    s.innerHTML = ICONS[name];
    return s;
  }

  /** Build an element. props: class, text, dataset, on*, attributes, or DOM properties. */
  function h(tag, props, ...kids) {
    const el = document.createElement(tag);
    if (props) {
      for (const [k, v] of Object.entries(props)) {
        if (v === undefined || v === null || v === false) continue;
        if (k === "class") el.className = v;
        else if (k === "text") el.textContent = v;
        else if (k === "dataset") Object.assign(el.dataset, v);
        else if (k === "style") for (const [p, val] of Object.entries(v)) el.style.setProperty(p, val);
        else if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
        else if (k === "value" || k === "checked" || k === "selected" || k === "disabled" || k === "indeterminate") el[k] = v;
        else el.setAttribute(k, v === true ? "" : String(v));
      }
    }
    for (const c of kids.flat(Infinity)) {
      if (c === null || c === undefined || c === false) continue;
      el.append(c instanceof Node ? c : String(c));
    }
    return el;
  }

  /** replaceChildren that skips null, undefined and false, like h(). */
  function fill(el, ...kids) {
    el.replaceChildren(...kids.flat(Infinity).filter((k) => k !== null && k !== undefined && k !== false));
    return el;
  }

  // ---- formatting -----------------------------------------------------------
  function clock(s) {
    if (s == null || !Number.isFinite(s)) return "–:––";
    s = Math.max(0, Math.round(s));
    const hh = Math.floor(s / 3600);
    const mm = Math.floor((s % 3600) / 60);
    const ss = String(s % 60).padStart(2, "0");
    return hh ? `${hh}:${String(mm).padStart(2, "0")}:${ss}` : `${mm}:${ss}`;
  }
  function length(s) {
    const m = Math.round(s / 60);
    if (m < 60) return `${m} min`;
    return `${Math.floor(m / 60)} h ${m % 60} min`;
  }
  /** Decimal units, the way sticks are sold: a "16 GB" stick shows as 15.6 GB. */
  function bytes(b) {
    if (b >= 1e9) return `${(b / 1e9).toFixed(b >= 1e11 ? 0 : b >= 1e10 ? 1 : 2)} GB`;
    if (b >= 1e6) return `${Math.round(b / 1e6)} MB`;
    if (b >= 1e3) return `${Math.round(b / 1e3)} KB`;
    return `${b} B`;
  }
  function ago(t) {
    const d = Date.now() - t;
    if (d < 60000) return "just now";
    if (d < 3600000) return `${Math.round(d / 60000)} min ago`;
    if (d < 86400000) return `${Math.round(d / 3600000)} h ago`;
    const days = Math.round(d / 86400000);
    return days === 1 ? "yesterday" : `${days} days ago`;
  }
  const plural = (n, one, many) => `${n} ${n === 1 ? one : many || one + "s"}`;
  const db = (v) => (v > 0 ? "+" : v < 0 ? "−" : "") + Math.abs(v).toFixed(1) + " dB";

  // ---- menus and popovers ---------------------------------------------------------
  const menuEl = () => document.getElementById("menu");
  let menuReturn = null;

  function place(el, anchor) {
    el.hidden = false;
    el.style.left = "0px";
    el.style.top = "0px";
    const r = anchor.getBoundingClientRect ? anchor.getBoundingClientRect() : { left: anchor.x, right: anchor.x, top: anchor.y, bottom: anchor.y };
    const w = el.offsetWidth;
    const hgt = el.offsetHeight;
    let x = anchor.getBoundingClientRect ? r.right - w : r.left;
    if (x < 8) x = Math.min(r.left, window.innerWidth - w - 8);
    x = Math.max(8, Math.min(x, window.innerWidth - w - 8));
    let y = r.bottom + 4;
    if (y + hgt > window.innerHeight - 8) y = Math.max(8, r.top - hgt - 4);
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
  }

  function closeMenu() {
    const m = menuEl();
    if (m.hidden) return;
    m.hidden = true;
    m.replaceChildren();
    m.className = "menu";
    if (menuReturn && document.contains(menuReturn)) menuReturn.focus({ preventScroll: true });
    menuReturn = null;
  }

  /** items: [{label, key, action, disabled, checked}] | "-" | {heading} */
  function menu(anchor, items) {
    closeMenu();
    const m = menuEl();
    m.setAttribute("role", "menu");
    for (const it of items) {
      if (it === "-") { m.append(h("hr")); continue; }
      if (it.heading) { m.append(h("div", { class: "mh", text: it.heading })); continue; }
      m.append(
        h("button", {
          type: "button", class: "mi", role: it.checked === undefined ? "menuitem" : "menuitemcheckbox",
          "aria-checked": it.checked === undefined ? null : String(!!it.checked), disabled: !!it.disabled,
          onclick: () => { closeMenu(); it.action(); },
        },
        it.checked === undefined ? null : h("span", { class: "tick" }, it.checked ? icon("check") : null),
        h("span", { text: it.label }), it.key ? h("span", { class: "k", text: it.key }) : null),
      );
    }
    menuReturn = document.activeElement;
    place(m, anchor);
    const first = m.querySelector(".mi:not(:disabled)");
    if (first) first.focus({ preventScroll: true });
  }

  function popover(anchor, content) {
    closeMenu();
    const m = menuEl();
    m.setAttribute("role", "dialog");
    m.className = "menu pop";
    m.append(content);
    menuReturn = document.activeElement;
    place(m, anchor);
    const f = m.querySelector("input, button, select");
    if (f) f.focus({ preventScroll: true });
  }

  document.addEventListener("pointerdown", (e) => {
    const m = menuEl();
    if (!m.hidden && !m.contains(e.target) && !e.target.closest("[data-menu-anchor]")) closeMenu();
  }, true);
  document.addEventListener("keydown", (e) => {
    const m = menuEl();
    if (m.hidden) return;
    if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); closeMenu(); return; }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const items = [...m.querySelectorAll(".mi:not(:disabled)")];
      if (!items.length) return;
      e.preventDefault();
      e.stopPropagation();
      const i = items.indexOf(document.activeElement);
      const n = e.key === "ArrowDown" ? (i + 1) % items.length : (i - 1 + items.length) % items.length;
      items[n].focus();
    }
  }, true);
  window.addEventListener("resize", closeMenu);

  // ---- dialog -------------------------------------------------------------------------
  let modalReturn = null;
  let modalOnClose = null;
  /** `tall` keeps the dialog one size whatever it shows (tabs). `player`
   * leaves the player bar usable above the dimmed window, for dialogs that
   * preview what they change. */
  function modal({ title, body, buttons, wide, tall, onClose, player }) {
    const back = document.getElementById("modal-back");
    const m = document.getElementById("modal");
    m.className = `modal${wide ? " wide" : ""}${tall ? " tall" : ""}`;
    m.replaceChildren(
      h("h2", { id: "modal-title", text: title }),
      h("div", { class: "mbody" }, body),
      h("div", { class: "mfoot" }, buttons),
    );
    modalReturn = document.activeElement;
    modalOnClose = onClose || null;
    document.body.classList.toggle("modal-player", !!player);
    // The dialog sits above the player bar, not over it.
    const bar = player && document.getElementById("player");
    back.style.paddingBottom = bar ? `${bar.offsetHeight + 16}px` : "";
    back.hidden = false;
    const f = m.querySelector("[data-autofocus]") || m.querySelector(".mfoot .btn:last-child");
    if (f) f.focus();
  }
  function closeModal() {
    const back = document.getElementById("modal-back");
    if (back.hidden) return;
    back.hidden = true;
    document.body.classList.remove("modal-player");
    document.getElementById("modal").replaceChildren();
    const cb = modalOnClose;
    modalOnClose = null;
    if (modalReturn && document.contains(modalReturn)) modalReturn.focus({ preventScroll: true });
    if (cb) cb();
  }
  const modalOpen = () => !document.getElementById("modal-back").hidden;
  document.addEventListener("keydown", (e) => {
    if (!modalOpen()) return;
    if (e.key === "Escape") { e.preventDefault(); closeModal(); return; }
    if (e.key === "Tab") {
      const f = [...document.getElementById("modal").querySelectorAll("button, input, select, textarea, [tabindex]")].filter((x) => !x.disabled && x.offsetParent);
      if (!f.length) return;
      const i = f.indexOf(document.activeElement);
      if (e.shiftKey && i <= 0) { e.preventDefault(); f[f.length - 1].focus(); }
      else if (!e.shiftKey && i === f.length - 1) { e.preventDefault(); f[0].focus(); }
    }
  });
  document.addEventListener("pointerdown", (e) => {
    if (e.target.id === "modal-back") closeModal();
  });

  // ---- toast ---------------------------------------------------------------------------
  let toastTimer = null;
  function toast(msg, action) {
    const t = document.getElementById("toast");
    t.replaceChildren(h("span", { text: msg }), ...(action ? [h("button", { type: "button", class: "btn", text: action.label, onclick: () => { hideToast(); action.run(); } })] : []));
    t.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(hideToast, action ? 7000 : 4000);
  }
  function hideToast() {
    document.getElementById("toast").hidden = true;
  }

  Fatstick.ui = { h, fill, icon, clock, length, bytes, ago, plural, db, menu, popover, closeMenu, modal, closeModal, modalOpen, toast };
})(window.Fatstick = window.Fatstick || {});
