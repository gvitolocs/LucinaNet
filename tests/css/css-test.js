// Shared harness for the cssRules tests in this directory. Results go to the console, which
// LUCINANET_CONSOLE=1 sends to stdout; a page ends in exactly one "css-test: PASS" or
// "css-test: FAIL ..." line, and the runner also fails a page whose line never arrives.
const cssTest = {
  log(line) {
    document.getElementById("out").appendChild(document.createTextNode(line + "\n"));
    console.log(line);
  },

  className(value) {
    if (value === null || value === undefined) return String(value);
    return Object.getPrototypeOf(value).constructor.name;
  },

  cssTextOf(rule) {
    try {
      return rule.cssText;
    } catch (e) {
      return "threw " + e.name;
    }
  },

  // What CSSRule promises a page, for any rule the engine kept: an object of a CSSRule subclass,
  // a numeric type, and a serialisation that says what the rule is. "" means it holds up.
  honest(rule) {
    if (!rule) return "index returned " + String(rule);
    const name = cssTest.className(rule);
    if (!/^CSS\w+Rule$/.test(name)) return name + " is not a CSSRule subclass";
    if (typeof rule.type !== "number") return "type is " + typeof rule.type;
    const css = cssTest.cssTextOf(rule);
    if (typeof css !== "string" || css.length === 0) return name + " serialised to " + JSON.stringify(css);
    try {
      void rule.parentStyleSheet;
      void rule.parentRule;
    } catch (e) {
      return "parent access threw " + e.name;
    }
    return "";
  },

  finish(test, problems) {
    cssTest.log(cssTest.line(test, problems));
  },

  line(test, problems) {
    return problems === 0
      ? "css-test: PASS " + test
      : "css-test: FAIL " + test + " (" + problems + " problem" + (problems === 1 ? "" : "s") + ")";
  },
};
cssTest.honest = cssTest.honest;
