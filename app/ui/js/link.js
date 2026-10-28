// The link between the main window and the stick layout windows: Tauri
// events in the app, a BroadcastChannel in a browser (the demo).
//
//   layout:want     {setId}        a layout window opened and wants its data
//   layout:data     {setId, ...}   the main window's plan of that stick
//   layout:gone     {setId}        a layout window closed
//   layout:rendered {setId, ...}   a layout window drew what it got (tests)
"use strict";
(function (P2U) {
  const T = window.__TAURI__;
  let bc = null;
  const chan = () => bc || (bc = new BroadcastChannel("p2u-layout"));

  function emit(name, payload) {
    if (T) return T.event.emit(name, payload);
    chan().postMessage({ name, payload });
    return Promise.resolve();
  }
  function on(name, cb) {
    if (T) return T.event.listen(name, (e) => cb(e.payload));
    const f = (m) => { if (m.data && m.data.name === name) cb(m.data.payload); };
    chan().addEventListener("message", f);
    return Promise.resolve(() => chan().removeEventListener("message", f));
  }
  /** Open (or bring forward) the layout window of a stick. */
  async function open(setId, title) {
    if (T) return T.core.invoke("open_layout", { setId, title });
    const w = window.open(`layout.html?set=${encodeURIComponent(setId)}`, `layout-${setId}`, "width=620,height=780");
    if (w) w.focus();
    return null;
  }

  P2U.link = { emit, on, open };
})(window.P2U = window.P2U || {});
