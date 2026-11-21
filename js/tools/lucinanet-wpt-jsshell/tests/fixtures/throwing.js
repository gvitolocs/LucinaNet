// A `script=` helper that throws while it loads, the way a WPT helper that reaches for a
// missing feature does.
throw new ReferenceError("this helper needs a global the engine does not have");
