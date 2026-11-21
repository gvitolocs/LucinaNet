// What the parity harness compares: run in Chromium and in any engine with JavaScript. Blitz writes the same JSON from Rust.
(function () {
  const boxes = [];
  const walk = (el, path) => {
    if (boxes.length >= 4000) return;
    const r = el.getBoundingClientRect();
    if (r.width > 0 && r.height > 0) {
      boxes.push({ p: path, x: r.x + scrollX, y: r.y + scrollY, w: r.width, h: r.height });
    }
    const seen = {};
    for (const child of el.children) {
      const tag = child.localName;
      seen[tag] = (seen[tag] || 0) + 1;
      walk(child, path + "/" + tag + "[" + seen[tag] + "]");
    }
  };
  if (document.documentElement) walk(document.documentElement, "html");
  return JSON.stringify({
    title: document.title,
    text: document.body ? document.body.innerText : "",
    elements: document.getElementsByTagName("*").length,
    boxes,
  });
})()
