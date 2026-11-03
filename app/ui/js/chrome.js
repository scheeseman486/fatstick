// The window's own title bar. The system draws none (decorations are off in
// tauri.conf.json), so the top bar moves the window, and the buttons are drawn here
// the way each system has them: coloured lights at the top left on a Mac, plain
// square ones in the corner on Windows, and round ones in KDE's style on Linux, at
// whichever end the desktop has them unless the Menu says otherwise. Resizing by the
// window's edges comes from Tauri on Windows and Linux, and from the system on a Mac.
//
// In a plain browser there's no window to look after, so nothing is drawn unless
// asked for with ?os= (for the tests and the screenshots).
"use strict";
(function (Fatstick) {
  const { h, fill } = Fatstick.ui;
  const svg = (d) => `<svg viewBox="0 0 12 12" fill="none" stroke="currentColor" aria-hidden="true">${d}</svg>`;
  const ICONS = {
    windows: {
      minimize: svg('<path d="M1 6h10"/>'),
      maximize: svg('<rect x="1.5" y="1.5" width="9" height="9"/>'),
      restore: svg('<path d="M3.5 3.5v-2h7v7h-2"/><rect x="1.5" y="3.5" width="7" height="7"/>'),
      close: svg('<path d="M1.5 1.5l9 9M10.5 1.5l-9 9"/>'),
    },
    // Breeze's: chevrons, a diamond when maximized, and a cross.
    linux: {
      minimize: svg('<path d="M2 4.2l4 4 4-4"/>'),
      maximize: svg('<path d="M2 7.8l4-4 4 4"/>'),
      restore: svg('<path d="M6 1.8 10.2 6 6 10.2 1.8 6z"/>'),
      close: svg('<path d="M2.4 2.4l7.2 7.2M9.6 2.4l-7.2 7.2"/>'),
    },
    // Shown in the lights when the pointer is over them.
    macos: {
      minimize: svg('<path d="M2.6 6h6.8"/>'),
      maximize: svg('<path d="M3.1 3.1h3.8L3.1 6.9zM8.9 8.9H5.1L8.9 5.1z" fill="currentColor" stroke="none"/>'),
      close: svg('<path d="M3.4 3.4l5.2 5.2M8.6 3.4l-5.2 5.2"/>'),
    },
  };

  Fatstick.chromeSetup = function ({ api, saveSetting }) {
    const root = document.documentElement;
    let C = null;
    let where = "auto";
    let state = { maximized: false, fullscreen: false };

    /** The buttons at each end: the desktop's, or the Menu's on Linux. */
    function layout() {
      if (C.os === "linux" && where === "left") return { left: ["close", "minimize", "maximize"], right: [] };
      if (C.os === "linux" && where === "right") return { left: [], right: ["minimize", "maximize", "close"] };
      return C;
    }
    function act(action) {
      api.windowAction(action).catch(() => {});
    }
    function button(kind) {
      const mac = C.os === "macos";
      const restore = kind === "maximize" && (state.maximized || state.fullscreen);
      const label = kind === "close" ? "Close" : kind === "minimize" ? "Minimize" : mac ? (state.fullscreen ? "Exit full screen" : "Full screen") : restore ? "Restore" : "Maximize";
      const b = h("button", {
        type: "button", class: `win-btn win-${kind}`, "aria-label": label, title: mac ? null : label,
        // No minimizing from full screen, as on any Mac.
        disabled: mac && kind === "minimize" && state.fullscreen,
        onclick: (e) => {
          if (kind === "maximize" && mac) act(e.altKey ? "maximize" : "fullscreen");
          else act(kind);
        },
      });
      b.innerHTML = ICONS[C.os][restore && !mac ? "restore" : kind];
      return b;
    }
    function draw() {
      if (!C) return;
      const { left, right } = layout();
      fill(document.getElementById("win-left"), left.map(button));
      fill(document.getElementById("win-right"), right.map(button));
      root.classList.toggle("win-max", state.maximized || state.fullscreen);
      root.dataset.winSides = `${left.length ? "l" : ""}${right.length ? "r" : ""}`;
    }
    async function look() {
      try {
        const now = await api.windowState();
        if (now.maximized !== state.maximized || now.fullscreen !== state.fullscreen) {
          state = now;
          draw();
        }
      } catch { /* it stays as it was */ }
    }
    /** Dragging the bar moves the window; pressing it twice maximizes it. Under a
     * dialog, the bar's part of the dimming still moves it. The move only starts once
     * the pointer does: handed over on the press, the window manager takes the mouse,
     * this page never hears the release, and a second press is never a double click.
     * A quick click could even leave some window managers moving the window with no
     * button held. */
    function onPress(e) {
      if (e.button !== 0) return;
      const under = e.target.id === "modal-back" && e.clientY < document.querySelector(".top").getBoundingClientRect().bottom;
      if (!under && (!e.target.closest(".top") || e.target.closest("button, input, select, a"))) return;
      e.preventDefault();
      if (e.detail === 2 && !under) {
        act("maximize");
        return;
      }
      const done = () => {
        document.removeEventListener("mousemove", onMove);
        document.removeEventListener("mouseup", done);
      };
      const onMove = (m) => {
        if (!(m.buttons & 1)) return done();
        if (Math.abs(m.screenX - e.screenX) < 3 && Math.abs(m.screenY - e.screenY) < 3) return;
        done();
        act("drag");
      };
      document.addEventListener("mousemove", onMove);
      document.addEventListener("mouseup", done);
    }
    async function load(setting) {
      if (["left", "right"].includes(setting)) where = setting;
      try {
        C = await api.windowChrome();
      } catch {
        C = null;
      }
      if (!C) return;
      root.dataset.os = C.os;
      draw();
      document.addEventListener("mousedown", onPress);
      let timer = 0;
      window.addEventListener("resize", () => { clearTimeout(timer); timer = setTimeout(look, 80); });
      window.addEventListener("blur", () => root.classList.add("win-away"));
      window.addEventListener("focus", () => root.classList.remove("win-away"));
      look();
    }
    /** For the Menu, on Linux only. */
    function menuItems() {
      if (!C || C.os !== "linux") return [];
      const item = (v, label) => ({ label, checked: where === v, action: () => { where = v; saveSetting("windowButtons", v); draw(); } });
      return [{ heading: "Window buttons" }, item("auto", "As the desktop has them"), item("left", "On the left"), item("right", "On the right"), "-"];
    }
    return { load, menuItems };
  };
})(window.Fatstick = window.Fatstick || {});
