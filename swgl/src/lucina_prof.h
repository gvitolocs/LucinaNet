/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

// LucinaNet: where swgl's time goes. Off unless LUCINANET_SWGL_PROFILE is set; then every 2 s it prints one line a
// key (shader and blend mode, or an operation) with calls, instances, shaded pixels and milliseconds, and resets.
// Composite runs on SwCompositor's thread too, so the table is under a lock.

#include <pthread.h>

namespace lucina_prof {

struct Slot {
  const char* name;
  int blend;
  uint64_t calls, instances, pixels, ns;
};

static const int kSlots = 256;
static Slot slots[kSlots];
static int used = 0;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static uint64_t last_dump = 0;
static int enabled = -1;

static void flush_at_exit();

static inline bool on() {
  if (enabled < 0) {
    enabled = getenv("LUCINANET_SWGL_PROFILE") ? 1 : 0;
    // The last window (a short replay has only that one) is printed at exit.
    if (enabled == 1) atexit(flush_at_exit);
  }
  return enabled == 1;
}

static void dump_locked(uint64_t now) {
  // Sort by time, largest first (insertion sort: a few dozen keys).
  for (int i = 1; i < used; i++) {
    Slot s = slots[i];
    int j = i - 1;
    while (j >= 0 && slots[j].ns < s.ns) {
      slots[j + 1] = slots[j];
      j--;
    }
    slots[j + 1] = s;
  }
  double total = 0;
  for (int i = 0; i < used; i++) total += slots[i].ns;
  fprintf(stderr, "swgl-prof window %.0f ms, swgl %.1f ms\n",
          double(now - last_dump) / 1e6, total / 1e6);
  for (int i = 0; i < used; i++) {
    Slot& s = slots[i];
    fprintf(stderr,
            "swgl-prof %-48s blend %3d calls %6llu inst %7llu px %10llu ms %8.2f\n",
            s.name, s.blend, (unsigned long long)s.calls,
            (unsigned long long)s.instances, (unsigned long long)s.pixels,
            double(s.ns) / 1e6);
  }
  used = 0;
  last_dump = now;
}

static uint64_t get_time_ns() {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return uint64_t(t.tv_sec) * 1000000000ull + t.tv_nsec;
}

static void flush_at_exit() {
  pthread_mutex_lock(&lock);
  if (used) dump_locked(get_time_ns());
  pthread_mutex_unlock(&lock);
}

static void add(const char* name, int blend, uint64_t instances,
                uint64_t pixels, uint64_t start, uint64_t end) {
  pthread_mutex_lock(&lock);
  if (!last_dump) last_dump = start;
  int i = 0;
  while (i < used && !(slots[i].name == name && slots[i].blend == blend)) i++;
  if (i == used && used < kSlots) {
    slots[i] = Slot{name, blend, 0, 0, 0, 0};
    used++;
  }
  if (i < used) {
    slots[i].calls++;
    slots[i].instances += instances;
    slots[i].pixels += pixels;
    slots[i].ns += end - start;
  }
  if (end - last_dump > 2000000000ull) dump_locked(end);
  pthread_mutex_unlock(&lock);
}

}  // namespace lucina_prof

#define LUCINA_PROF_BEGIN() \
  uint64_t lucina_prof_start = lucina_prof::on() ? get_time_value() : 0
#define LUCINA_PROF_END(name, blend, instances, pixels)                    \
  do {                                                                     \
    if (lucina_prof_start)                                                 \
      lucina_prof::add(name, blend, instances, pixels, lucina_prof_start,  \
                       get_time_value());                                  \
  } while (0)
