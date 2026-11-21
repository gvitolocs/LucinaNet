# edge-wrap OOM pages

Pages for the probe binary run with LUCINANET_JS_MAX_HEAP_MB=64 (CI: the "OOM pages (edge-wrap)" step of
.github/workflows/ci.yml, which runs every tests/edge-wrap/*.html). Each page exhausts the capped JS heap at a
different engine entry point:

- oom-microtask.html: inside a promise reaction chain, so the context is poisoned during a microtask checkpoint.
- oom-raf.html: inside requestAnimationFrame callbacks, so it is poisoned during "update the rendering".
- oom-worker.html: inside a dedicated worker (data: URL) message handler and its microtasks.

Expected result for every page: the render exits 0 (a crash is 139) and the log has one record
"context poisoned: OutOfMemory". Run: LUCINANET_JS_MAX_HEAP_MB=64 lucinanet-servo --render file://$PWD/tests/edge-wrap/oom-microtask.html out.png
