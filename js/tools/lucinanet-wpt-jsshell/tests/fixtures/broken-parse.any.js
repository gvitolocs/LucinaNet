// META: global=jsshell

// A file the engine will not even parse. Nothing registers, so there is no completion to
// wait for: the runner reports it and moves on instead of burning the whole timeout.
let missing = = 1;
