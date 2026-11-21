// Read-only check for tests/web/github-home.sh. It answers one question in one line: on the signed-out
// https://github.com/ home, has the React-hydrated content actually painted -- the hero text and the live
// activity feed -- or is the area below the header still the empty box the 2026-11-13 30-site sweep saw?
//
// Why those four readings: GitHub server-renders the whole marketing home inside <react-app> (378 KB, ~2200
// elements, the feed as DataMarquee/Cell/RepositoryPanel), but it ships the hero with inline
// style="opacity:0" and only React's first client render clears it. So "the DOM is there" is not the test --
// the DOM is always there -- "the hero is opaque and the feed has painted boxes" is.
//
// Nothing is changed: it reads the DOM and the computed styles and prints. Lines start "ghhome".
(() => {
  if (window.__ghhomeCheck) return;
  window.__ghhomeCheck = true;

  const say = (w) => { try { console.log(`ghhome ${w}`); } catch (e) {} };

  const num = (s) => { const m = /(-?[\d.]+)/.exec(String(s)); return m ? parseFloat(m[1]) : 0; };

  // First element matching any of these class fragments; "" when none.
  const find = (fragments) => {
    for (const f of fragments) {
      const el = document.querySelector(`[class*="${f}"]`);
      if (el) return el;
    }
    return null;
  };

  const read = (el) => {
    if (!el) return { present: 0, kids: 0, h: 0, w: 0, op: "" };
    let cs = null, rect = null;
    try { cs = getComputedStyle(el); } catch (e) {}
    try { rect = el.getBoundingClientRect(); } catch (e) {}
    return {
      present: 1,
      kids: el.childElementCount,
      h: rect ? Math.round(rect.height) : 0,
      w: rect ? Math.round(rect.width) : 0,
      op: cs ? cs.opacity : "?",
    };
  };

  const verdict = () => {
    const main = document.querySelector("main");
    const reactApp = document.querySelector("react-app");
    const root = document.querySelector('[data-target="react-app.reactRoot"]');

    // The hero: the SSR'd block that carries inline opacity:0 until React's first client render.
    const hero = find(["Intro__content", "Intro_"]);
    // The live activity feed: the marquee of "repo forked from ...", "repo starred ...", "branch merged ..."
    // cells, and the repository panels it scrolls.
    const marquee = find(["DataMarquee"]);
    const panel = find(["RepositoryPanel"]);
    const cells = document.querySelectorAll('[class*="DataMarquee"] [class*="Cell"], [class*="Cell"]');

    const rMain = read(main), rRoot = read(root), rHero = read(hero),
          rMarq = read(marquee), rPanel = read(panel);

    const upgraded = !!(reactApp && window.customElements && customElements.get("react-app") &&
                        reactApp.constructor && reactApp.constructor.name !== "HTMLElement");

    // The four things that together say "the feed area painted": React took over, the hero is opaque,
    // the marquee has children in a box with height, and cells exist with painted boxes.
    // Which page this is decides what "painted" can mean: the marketing home is the React page under test, the
    // repo page is only a control and carries none of the marketing classes.
    const page = location.pathname === "/" ? "home"
        : (/^\/[^/]+\/[^/]+/.test(location.pathname) ? "repo" : "other");

    const heroOpaque = rHero.present && parseFloat(rHero.op) >= 0.99;
    const feedPainted = rMarq.present && rMarq.kids > 0 && rMarq.h > 20;
    const cellsPainted = cells.length > 0;
    const mainTall = rMain.present && rMain.h > 1000;

    const ok = page === "home"
        ? (upgraded && heroOpaque && feedPainted && cellsPainted && mainTall ? 1 : 0)
        : 1;

    say(`VERDICT page=${page} ok=${ok} upgraded=${upgraded ? 1 : 0} heroOp=${rHero.op || "none"} ` +
        `heroBox=${rHero.w}x${rHero.h} feedKids=${rMarq.kids} feedBox=${rMarq.w}x${rMarq.h} ` +
        `cells=${cells.length} panelBox=${rPanel.w}x${rPanel.h} mainBox=${rMain.w}x${rMain.h} ` +
        `rootKids=${rRoot.kids} els=${document.querySelectorAll("*").length} ready=${document.readyState}`);
    // Readings that mean the same thing on any GitHub page, so the control can be scored on its own terms:
    // the document's own content sits in a box with height and enough elements painted it.
    say(`CONTENT mainKids=${rMain.kids} mainBox=${rMain.w}x${rMain.h} els=${document.querySelectorAll("*").length} ` +
        `bodyBox=${read(document.body).w}x${read(document.body).h}`);
    if (!ok) {
      say(`WHY upgraded=${upgraded ? 1 : 0} heroPresent=${rHero.present} heroOpaque=${heroOpaque ? 1 : 0} ` +
          `feedPresent=${rMarq.present} feedPainted=${feedPainted ? 1 : 0} cellsPainted=${cellsPainted ? 1 : 0} ` +
          `mainTall=${mainTall ? 1 : 0}`);
    }
  };

  // Several shots: the reveal lands ~11-13 s after the script starts, and the harness snapshots whenever it
  // settles, so the last line printed before the exit is the one that matters.
  for (const at of [4000, 8000, 12000, 16000, 20000, 24000]) setTimeout(verdict, at);
  addEventListener("load", () => setTimeout(verdict, 2000));
  say("check installed");
})();
